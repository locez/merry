# Merry

Merry is a Rust-first agent runtime for long-lived, tool-using model sessions.
It keeps provider wire formats outside the runtime, streams model output as it
arrives, records tool evidence as artifacts, and exposes the same runtime
through a terminal client, Rust APIs, and Python bindings.

Two commitments shape the design:

- **Runtime-first state.** Sessions, ledger facts, artifacts, context
  compilation, checkpoints, cancellation, and permissions are owned by the
  runtime. Raw chat history is not the source of truth.
- **An explicit action boundary.** Every process action runs inside the sandbox
  mode you choose, under a permission ceiling that approval cannot widen, with
  host integrations enabled one capability at a time.

## Status And Focus

Merry is under active development and has no released version yet. T0 through T8
and the T10 architecture split are complete: the runtime, CLI, TUI, Rust facade,
and Python SDK run end to end, backed by an offline deterministic test suite and
CI gates; live provider checks are opt-in. One milestone remains open:

| Milestone | State |
|---|---|
| T11 release gates and Definition of Done | open |

The current focus is release readiness, not new runtime surface.
[ROADMAP.md](ROADMAP.md) keeps the delivery sequence, its dependencies, and
links to the tracking issues.

## What Works

- Real streaming from provider SSE through retry, runtime events, CLI, TUI, and
  Python async iteration.
- OpenAI-compatible `responses` and `chat_completions` protocols.
- Anthropic Messages streaming with text, tool use, usage, and stop-reason
  normalization.
- Named providers and per-role provider/model selection, including the
  `context_compaction` and `approval_review` roles.
- Multiple tool calls in one model turn with ordered continuation results.
- Explicit tool concurrency: consecutive `parallel_safe` calls use bounded
  concurrency; `exclusive` calls are barriers. Tools are exclusive by default.
- Runtime-owned sessions, ledger facts, artifacts, context compilation,
  checkpoints, cancellation, permission review, and structured final output.
- A sandbox and permission model where approval cannot widen the configured
  ceiling and host integrations stay opt-in per capability.
- A responsive terminal timeline with live deltas, compact tool activity,
  queues, completion, resume, and a balanced magenta theme. Detailed session
  inspection is provided by the local Web trajectory page.
- Thin Rust and Python facades over the same Rust-owned runtime.

## Quick Start

Requirements:

- Stable Rust with Rust 2024 edition support.
- Linux 5.13 or newer for the default workspace sandbox (Landlock). Pass
  `--sandbox off` on a host that cannot enforce it. See [SANDBOX.md](SANDBOX.md).
- Node.js and npm when building the local Web trajectory application.

Build:

```bash
# Build the Web assets that are embedded into the binary
cd web
npm ci
npm test
cd ..

cargo build --release -p merry-cli
```

The Web application is built into the ignored `web/dist/` directory and is
embedded into the Rust binary by `merry-web` at Cargo build time, so a Rust build
fails with an explicit message until those assets exist. `npm test` type-checks
the TypeScript and runs the Web tests; `cargo build` does not invoke Node or
rebuild the Web assets. CI verifies that the generated contract source is
current.

When changing the Rust trajectory contract, refresh its canonical schema before
running the Web checks:

```bash
cargo run -p merry-core --example trajectory-schema --quiet > crates/merry-core/schema/trajectory-event.json
(cd web && npm test)
```

Configure at least one provider ([Configure](#configure)), then run a task. The
binary is `target/release/merry`:

```bash
# Headless coding task; sandboxed by default
target/release/merry run "fix the failing tests"

# Machine-readable runtime events
target/release/merry run --events-jsonl "inspect the current failure"

# Interactive streaming TUI; sandboxed by default
target/release/merry
```

[Use The CLI](#use-the-cli) documents the full command surface, including
session resume, stdin tasks, and shell completions.

## Configure

Merry reads:

```text
$XDG_CONFIG_HOME/merry/config.toml
fallback: ~/.config/merry/config.toml
```

Start from [`examples/config.toml`](examples/config.toml). Keep API keys in a
separate file referenced by `api_key_file`; paths are relative to the config
directory.

### OpenAI Responses

```toml
[providers.default]
provider = "openai-compatible"
model = "gpt-4.1-mini"

[providers.openai-compatible]
type = "openai-compatible"
protocol = "responses"
base_url = "https://api.openai.com/v1"
api_key_file = "secrets/openai.key"
```

### OpenAI Chat Completions

Use this for compatible vendors that do not expose the Responses API:

```toml
[providers.default]
provider = "compat"
model = "vendor-model"

[providers.compat]
type = "openai-compatible"
protocol = "chat_completions"
base_url = "https://provider.example/v1"
api_key_file = "secrets/provider.key"
```

### Anthropic Messages

```toml
[providers.default]
provider = "anthropic"
model = "claude-sonnet-4-5"

[providers.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
api_version = "2023-06-01"
default_max_output_tokens = 4096
api_key_file = "secrets/anthropic.key"
```

Model roles can select a different provider without leaking provider-specific
types into the runtime:

```toml
[models.context_compaction]
provider = "openai-compatible"
model = "gpt-4.1-mini"
```

### MCP Availability

Configured HTTP MCP servers are optional at startup: connection, authentication,
and remote-protocol failures produce visible TUI warnings or headless stderr
warnings while Merry continues. Invalid local configuration still fails validation.
Discovery allows up to four concurrent servers, five seconds per server, and ten
seconds overall. Headless JSONL output remains machine-readable.

Each session freezes its external tool definitions, bindings, and order before
the first model request. Resume restores that catalog even when a server is
offline. Available executors may reconnect on use after a five-second cooldown.
If an MCP endpoint returns HTTP 404 for an existing transport session, Merry
treats that session as expired, rebuilds it on a later use, and does not replay
the failed call;
known-unavailable tools return recorded failures rather than leaving unresolved
calls. Timed-out tool calls are not replayed: their side effects may be unknown.
Authentication failures require correcting credentials and resuming the session;
they are not retried on every tool invocation.

New tools and incompatible definitions require a new session. Removing a server,
narrowing its allowlist, or changing its endpoint disables affected saved tools
without deleting their provider-visible definitions. A session that first starts
without any known definitions does not silently gain tools after reconnecting.
This prevents connectivity-induced prompt-prefix changes, not provider cache
expiration or changes to other parts of the prompt.

### Session State

Session state uses format 5 and requires an external tool catalog, even when it
is empty. Other formats are rejected without migration; Merry is not yet released
and does not support historical session formats. Start a new session if a saved
session uses an unsupported format. Catalogs do not store authentication headers
or MCP transport session IDs.

## Use The CLI

```bash
# Interactive streaming TUI; sandboxed by default
target/release/merry

# Resume a saved TUI session
target/release/merry resume

# Headless coding task; sandboxed by default
target/release/merry run "fix the failing tests"

# Machine-readable runtime events
target/release/merry run --events-jsonl "inspect the current failure"

# Read the task from stdin instead of argv
printf '%s' "$task" | target/release/merry run -

# Name a headless session, then continue it in a later run
target/release/merry run --session-id migration-1 "start the migration"
target/release/merry run --resume migration-1 "now update the tests"

# Generate a command plan without executing it
target/release/merry cmd "find the largest Rust files"

# Shell completions (bash, zsh, fish, elvish, powershell), generated from the
# same clap definition that parses the flags above
target/release/merry completions zsh > "${fpath[1]}/_merry"
target/release/merry completions bash > ~/.local/share/bash-completion/completions/merry
target/release/merry completions fish > ~/.config/fish/completions/merry.fish
```

`merry completions <SHELL>` prints the script to stdout and exits without
reading `config.toml` or starting a sandbox, so it works before Merry is
configured. Regenerate it after upgrading; the flags, subcommands, and value
lists such as `--approval-policy` are read from the binary itself.

Every `merry run` saves its session state under
`$XDG_STATE_HOME/merry/sessions/<session-id>` when the run settles, so a later
`--resume <session-id>` continues that session's ledger, transcript, artifacts,
and checkpoints. Without `--session-id` the run generates its own id, which the
`--events-jsonl` stream reports as `source.session_id` on every event. Headless
sessions carry no TUI metadata and so do not appear in the `merry resume`
picker; address them by id.

`--session-id` starts a session, so it refuses an id the store already holds
(exit 2) rather than replacing that session's saved state. Continue an existing
session with `--resume <session-id>` instead.

A `TASK` of `-` reads the task text from stdin. Prefer it for generated or
long prompts: an argv task is visible to every process listing on the host and
is bounded by the kernel's per-argument limit. Empty or whitespace-only stdin
is rejected as a usage error (exit 2).

Reading the task from stdin consumes that stream, so `merry run -` answers
permission review on the controlling terminal rather than on stdin; piping
approval answers alongside the task does not work. When the process has no
controlling terminal at all, each request is denied with that reason on stderr,
so grant the capabilities up front, pass the task on argv, or pick
`--auto-fallback deny` or `--permission-mode deny` for that run.

## Sandbox And Permissions

Each process action runs in the sandbox mode you choose. Merry itself stays on
the host, so model and MCP connections are not part of that switch. The `[cli]`
table sets the defaults:

```toml
[cli]
# sandbox = "workspace" or "off". Omitted means workspace.
permission_mode = "auto"
auto_fallback = "ask"
```

### Sandbox Mode

`--sandbox workspace` (the default) confines each action with Landlock on Linux.
Network stays closed until that action is approved. `--sandbox off` runs the
action on the host. A flag replaces the configured value. If the kernel cannot
enforce the workspace sandbox, startup stops and tells you to pass
`--sandbox off`. See [SANDBOX.md](SANDBOX.md).

### Permission Mode

`--permission-mode` names who reviews a request. A denial comes back to the
working model as a failed tool result. It does not end the turn by itself.

| Value | Reviewer |
|---|---|
| `auto` (default) | the `[models.approval_review]` model decides first |
| `ask` | you decide |
| `always-approve` | no review round |
| `deny` | the request is refused and returned to the model |

`auto_fallback` applies only when the mode is `auto`. `ask` (the default) asks
you when the review model denies or cannot decide. `deny` returns that denial
to the working model. `--always-approve` is the same as
`--permission-mode always-approve` and conflicts with `--permission-mode`.

`sandbox` and `permission_mode` are independent. `--sandbox off` with
`--always-approve` is the unsandboxed, no-prompt mode, and it is not the default.

### Permissions

`[permissions] network` defaults to on. A sandboxed action may request network,
and the request still goes through `permission_mode`. `network = false` rejects
the request before review, and approval cannot open it. The setting does not
restrict model-provider or configured MCP connections. `--sandbox off` does not
use this limit.

`readonly_paths`, `readwrite_paths`, and `deny_paths` are the same kind of
option for filesystem access. Trusted paths are available to ordinary actions
at the declared level. `review_paths` asks again for each action. `deny_paths`
is never reopened by an approval. A deny or review path inside an allowed
directory cannot be subtracted, so that action fails closed instead of running
unsandboxed.

`[permissions].environment` applies only to Merry-managed action processes.

### Host Integrations

`ssh_agent`, `gpg_agent`, and `dbus` preauthorize the matching socket when the
endpoint exists. SSH also grants existing `known_hosts` files read-only. GnuPG
also grants existing public keyrings read-only. Private keys stay off the list.
An integration that trusted configuration did not enable can still be requested
for one action when its endpoint is visible.

### How The Boundary Works

Read-write on first open makes the workspace writable and loads `AGENTS.md`.
Read-only leaves that directory read-only and does not load `AGENTS.md`. The
dialog can save the choice or ask again next time, and exit leaves without
opening it. System directories such as `/usr` and
`/bin`, a small set of `/etc` files, and the existing Git user files
(`~/.gitconfig` and `~/.config/git/{config,ignore,attributes}`) are readable.
`/tmp`, `/var/tmp`, the Cargo registry and git database, and the XDG cache are
writable. The rest of the home directory is traverse-only, so secrets such as
`~/.ssh` private keys, `~/.gnupg` private material, `~/.aws`, and
`~/.git-credentials` are not readable. Merry's own config and state directories
are denied to actions. A saved read-write or read-only choice lives under
Merry's state directory.

Until an action is approved for network, its IPv4, IPv6, and packet sockets are
rejected. Filesystem policy is an allow list. A `deny_paths` or `review_paths`
entry inside an allowed directory does not start the action; the error names
both entries. Landlock also does not hide names or cover every metadata
operation. Details are in [SANDBOX.md](SANDBOX.md).

## Multi-Tool Execution

Merry does not inspect shell text to guess whether calls are safe to overlap.
Each registered tool declares one runtime-owned contract:

- `ParallelSafe`: may overlap with adjacent parallel-safe calls.
- `Exclusive`: waits for earlier calls and blocks later calls until complete.

The model may still request any number of tools in one turn. Merry executes
parallel-safe waves with a bounded limit, preserves exclusive barriers, and
returns every result to the provider in the model's original call order.

## Embed Merry

### Rust

The `merry` crate is the public Rust facade. Provider builders produce the same
provider-neutral component used by `RuntimeBuilder`:

```rust
use merry::{Runtime, SessionId, providers::{RuntimeBuilderProviderExt, anthropic}};

fn build_runtime() -> Result<Runtime, Box<dyn std::error::Error>> {
    let provider = anthropic()
        .api_key("sk-ant-...")?
        .model_name("claude-sonnet-4-5")?
        .build()?;

    Ok(Runtime::builder(SessionId::new("example")?)
        .with_provider(provider)
        .build()?)
}
```

### Custom Rust Tools

Declare custom tools through `merry::tool`; applications do not need to depend
directly on `merry-tools` or `merry-tools-macros`. For an external project, add
these dependencies, adjusting the path to your local Merry checkout:

```toml
[dependencies]
merry = { path = "../merry/crates/merry" }
serde = { version = "1", features = ["derive"] }
schemars = { version = "1", features = ["derive"] }
```

A minimal tool has a typed input and an async handler:

```rust
use std::convert::Infallible;

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GreetInput {
    /// The person's name or preferred form of address, as provided by the user.
    name: String,
}

#[merry::tool(description = "Generate a greeting for a person by name.")]
async fn greet(input: GreetInput) -> Result<String, Infallible> {
    Ok(format!("Hello, {}!", input.name))
}
```

The macro generates `greet_tool()`. Register its result explicitly when building
an agent, using your application's session ID and configured provider:

```rust
let agent = merry::AgentBuilder::new(session_id)
    .provider(provider)
    .tool(greet_tool()?)
    .build()?;
```

- The generated factory has the same visibility as its handler.
- The tool name defaults to the handler name (`greet`). Override it with
  `#[merry::tool(name = "say_hello", description = "...")]`; the factory is still
  named `greet_tool()`.
- The required tool `description` explains what the tool does and when to use it.
- Field `///` comments become JSON Schema `description` values visible to the
  model. Describe each parameter's meaning and, where relevant, its format,
  units, constraints, and behavior when omitted. Merry does not currently require
  field descriptions, but they should be part of every tool declaration.
- Use `#[schemars(description = "...")]` when the model-facing field description
  should differ from its Rustdoc; the explicit description overrides the comment.

Runtime-owned tools that need a custom executor can annotate the input struct
instead. This generates only the schema factories; execution, policy, tracing,
and cancellation stay explicit:

```rust
#[merry::tool(
    name = "inspect_workspace",
    description = "Inspect one bounded workspace path."
)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InspectWorkspaceInput {
    #[schemars(description = "Workspace-relative path to inspect.")]
    path: String,
}

let spec = InspectWorkspaceInput::tool_spec()?;
let role_specific_spec = InspectWorkspaceInput::tool_spec_with(
    "inspect_workspace",
    "Inspect one bounded workspace path for the current role.",
)?;
```

Use the generated `ToolSpec` with an explicitly constructed runtime
`RegisteredTool` when the executor must remain runtime-owned.

Low-level executors can keep full display/evidence content in
`merry_runtime::ToolExecutionOutcome` and supply a distinct model body with
`with_model_text` or `with_model_json`. The model body must retain actionable
results and completeness warnings. Runtime persists it for replay and compaction;
artifact reads and checkpoint references still return the original full result.
Without a separate model body, tools keep their existing output behavior.
Process model bodies preserve captured UTF-8 stdout and stderr unchanged, adding
only status, stream labels, and applicable completeness or recovery warnings.
Non-UTF-8 streams use the same text body, explicitly marking invalid UTF-8 as
replacement characters (`�`) and advising byte-level inspection with a hex dump
or format-aware tool. Full artifacts retain the exact captured bytes; model
bodies do not repeat the JSON envelope or base64 encoding. Output from binary
inspection tools is returned unchanged rather than summarized by runtime.

Merry derives the input schema, decodes arguments, and serializes the handler's
return value. `Infallible` means this example cannot return a domain error; use
your application's error type for fallible handlers. The macro does not register
tools globally or change execution policy. Renamed Cargo dependencies are supported
automatically, and `crate = "path"` can select an explicit tool API boundary.
Return types may use aliases of `Result`.

### Python

Python bindings live in [`sdks/python`](sdks/python). They expose the same
Rust-owned agent lifecycle through typed async messages:

```python
agent = (
    merry.Agent.builder("example")
    .provider(
        merry.Anthropic(
            api_key="sk-ant-...",
            model="claude-sonnet-4-5",
        )
    )
    .build()
)
run = agent.stream("Inspect the repository")

async for message in run:
    if isinstance(message, merry.Event):
        print(message.type.value)

result = await run.result()
```

## Verify

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

CI runs the same Rust checks through the Deterministic Reliability Gate, together
with the Web contract check and the Python SDK workflow.

## Documentation

- [ROADMAP.md](ROADMAP.md) — delivery sequence, dependencies, and current focus.
- [SANDBOX.md](SANDBOX.md) — host setup and the full sandbox/permission reference.
- [AGENTS.md](AGENTS.md) — architecture ownership, dependency direction, and
  contributor contracts.
- [`sdks/python/README.md`](sdks/python/README.md) — Python development and
  verification.
- [`sdks/python/CAPABILITY_MATRIX.md`](sdks/python/CAPABILITY_MATRIX.md) — Rust
  and Python capability parity.
- [`examples/config.toml`](examples/config.toml) — annotated configuration
  starting point.
