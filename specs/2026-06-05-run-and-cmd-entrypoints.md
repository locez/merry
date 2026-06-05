# Run And Cmd Entrypoints

Date: 2026-06-05

## Purpose

Merry now has one profile-driven runtime construction path. The next product
entrypoints should consume that path instead of inventing another runner,
factory, or debug-only assembly layer.

This spec defines the boundary between two headless CLI entrypoints:

- `merry run`: complete a user task.
- `merry cmd`: produce a command or command plan.

Both commands may run multi-turn agent loops. Their difference is output
contract and default capability profile, not turn count.

## Shared Runtime Boundary

Both entrypoints should build runtimes through the same shape:

```rust
let profile = RuntimeProfile::builder()
    .with_workspace_coding_loop(workspace_profile)?
    .build()?;

let runtime = Runtime::builder(session_id)
    .automatic_compaction(compaction)
    .model_provider(provider, model)
    .with_profile(profile)?
    .build()?;
```

The CLI may own host concerns such as config loading, workspace root selection,
sandbox bootstrap, output formatting, and command-line flags. It must not own
agent-loop semantics, tool continuation, context compaction, event protocols,
or final result semantics.

## `merry run`

`merry run` is the headless coding-agent entrypoint.

Product contract:

- user intent is a task to complete
- final answer reports task outcome
- tools are means, not the output product
- the command may read, edit, run verification, and request permissions through
  runtime policy

First-version profile:

- workspace read/list/search tools
- opt-in workspace patch tool
- process tool with coding-agent process lanes
- permission request tool when a permissioned process runner factory exists
- configured skills and subagents
- automatic compaction from config
- coding-agent model-turn budget

First-version behavior:

```bash
merry run "fix the failing test"
```

- loads provider/model config from XDG config
- resolves current directory as the default workspace root
- builds the coding-agent `RuntimeProfile`
- runs `Runtime::run_agent_loop_stream`
- streams runtime events in a compact headless format
- prints terminal status and final output

Non-goals:

- do not add a `SessionRunner`
- do not add TUI state management
- do not add command-plan structured final output
- do not duplicate debug smoke fixtures or assertions

## `merry cmd`

`merry cmd` is a command-generation entrypoint.

Product contract:

- user intent is to receive a command or command plan
- final answer must be a structured command plan
- the runtime may inspect the project to make the command accurate
- default behavior must not execute the generated command

This command may also be multi-turn. Complex command generation often benefits
from reading project files, searching the workspace, checking package layout,
or running safe read-only discovery commands. The boundary from `merry run` is
that `merry cmd` stops when it can return a reliable command plan; it does not
complete the underlying task.

First-version profile:

- workspace read/list/search tools
- read-only process lane only
- no workspace patch tool
- no accepted local workspace process write/effect lane
- path access restricted by configured runtime capabilities
- network disabled unless explicitly allowed by config/profile

First-version behavior:

```bash
merry cmd "search the current project for all TypeScript test files"
```

The final output should be a typed command plan, not free text. A minimal shape:

```json
{
  "cwd": ".",
  "shell_command": "rg --files -g '*.{test,spec}.ts' -g '*.{test,spec}.tsx'",
  "risk": "read_only",
  "assumptions": ["ripgrep is available on PATH"],
  "notes": ["Searches tracked and untracked files visible from the current directory."]
}
```

The exact Rust type name can be `CommandPlan`. It should use structured final
output rather than relying on plain assistant text. `shell_command` is the
single executable command representation. The first version should not split
between argv execution and shell execution.

After generating the plan, an interactive TTY may offer to execute the command:

```text
Command:
  rg --files -g '*.{test,spec}.ts' -g '*.{test,spec}.tsx'

Risk: read_only

Notes:
  - Searches tracked and untracked files visible from the current directory.

Execute this shell command on your host? [y/N]
```

If the user confirms, the CLI executes the final command directly on the host:

```rust
Command::new("sh")
    .arg("-c")
    .arg(plan.shell_command)
    .current_dir(plan.cwd)
```

Confirmed execution is not a runtime tool call. It does not use Merry process
policy, does not create runtime artifacts, does not truncate output, and does
not send execution output back to the model. Stdout and stderr should stream
directly to the user's terminal. If Merry itself is running inside an outer
sandbox, that outer sandbox still naturally constrains the host shell process.

Non-interactive use must not prompt or execute by default. `--json` should emit
only the `CommandPlan` JSON and must not prompt or execute.

Non-goals:

- do not implement `merry cmd --run` in the first version
- do not execute without explicit interactive confirmation
- do not implement separate argv and shell execution paths
- do not use `merry cmd` as a smaller `merry run`
- do not expose raw provider output as the command plan

## Overlap Rules

These prompts are intentionally different:

```bash
merry cmd "find all test files"
merry run "find all test files"
```

`merry cmd` should return a command plan for finding them.

`merry run` should find them and report the result.

If a user asks `merry run` for a command, `merry run` may answer with a command
because that is the requested task result. If a user asks `merry cmd` to perform
the task, `merry cmd` should still return the command plan and state that it did
not execute the final command.

## Acceptance

`merry run` first slice:

- parses `merry run <task>`
- builds runtime through `RuntimeProfile` and `RuntimeBuilder::with_profile`
- uses the same coding-agent profile path as debug coding-loop smokes
- runs `Runtime::run_agent_loop_stream`
- prints runtime events and terminal result without debug fixture assumptions
- has deterministic tests with scripted providers

`merry cmd` first slice:

- parses `merry cmd <request>`
- builds runtime through `RuntimeProfile` and `RuntimeBuilder::with_profile`
- uses a read-only command-generation profile
- runs a multi-turn loop when tools are requested
- forces structured final output as `CommandPlan`
- represents the executable plan as `shell_command`
- prompts on interactive TTY before executing the generated command
- executes confirmed commands through host `sh -c` without runtime artifacts or
  truncation
- never prompts or executes under `--json` or non-interactive stdout/stdin
- has deterministic tests proving the final structured command plan
