# Merry

Merry is a Rust-first agent runtime for long-lived, tool-using model sessions.
It keeps provider wire formats outside the runtime, streams model output as it
arrives, records tool evidence as artifacts, and exposes the same runtime
through a terminal client, Rust APIs, and Python bindings.

The project is under active development. The default test suite is offline and
deterministic; live provider checks are opt-in.

## What Works

- Real streaming from provider SSE through retry, runtime events, CLI, TUI, and
  Python async iteration.
- OpenAI-compatible `responses` and `chat_completions` protocols.
- Anthropic Messages streaming with text, tool use, usage, and stop-reason
  normalization.
- Named providers and per-role provider/model selection.
- Multiple tool calls in one model turn with ordered continuation results.
- Explicit tool concurrency: consecutive `parallel_safe` calls use bounded
  concurrency; `exclusive` calls are barriers. Tools are exclusive by default.
- Runtime-owned sessions, ledger facts, artifacts, context compilation,
  checkpoints, cancellation, permission review, and structured final output.
- A responsive terminal timeline with live deltas, compact tool activity,
  queues, completion, resume, and a balanced magenta theme. Detailed session
  inspection is provided by the local Web trajectory page.
- Thin Rust and Python facades over the same Rust-owned runtime.

## Build

Requirements:

- Stable Rust with Rust 2024 edition support.
- Linux and `bubblewrap` for the default TUI and `merry run` sandbox.
- Node.js and npm when building the local Web trajectory application.

The Web application is built into the ignored `web/dist/` directory and is
embedded into the Rust binary by `merry-web` at Cargo build time. Build it
before compiling a Web-enabled Rust target:

```bash
cd web
npm ci
npm test
cd ..
cargo build --release -p merry-cli
```

`npm test` type-checks the TypeScript and runs the Web tests. `cargo build`
embeds the files from `web/dist/` into `target/release/merry`; it does not
invoke Node or rebuild the Web assets. CI verifies that the generated contract
source is current.

When changing the Rust trajectory contract, refresh its canonical schema before
running the Web checks:

```bash
cargo run -p merry-core --example trajectory-schema --quiet > crates/merry-core/schema/trajectory-event.json
(cd web && npm test)
```

The binary is `target/release/merry`.

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

Session state uses format 4 and requires an external tool catalog, even when it
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
```

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
permission review on the controlling terminal rather than on stdin. Piping
approval answers alongside the task does not work: they would be read as part
of the task. When the process has no controlling terminal, review has no way to
ask and each request is denied with that reason on stderr, so grant the
capabilities up front or pass the task on argv when a piped run needs approvals.

TUI and `run` use outer+inner bubblewrap automatically. `--inner-sandbox`
selects the Codex-compatible single inner sandbox, while `--no-sandbox`
selects the explicit unrestricted host mode: process actions inherit the host
filesystem, environment, and permissions without any bubblewrap namespace.
Both inner modes start from a read-only view of their parent filesystem, so
ordinary commands can see host configuration and toolchains. The inner action
policy controls workspace writes, network access, path review, and modeled host
integrations (SSH agent, native GPG agent, and D-Bus session bus). It masks known
unapproved socket endpoints, including endpoints under `/tmp`, rather than
hiding their entire shared parent directory. It is not a general IPC filter:
unmodeled Unix sockets visible through the inherited filesystem may remain
reachable even with read-only mounts and network isolation. In the default
outer+inner mode, the outer sandbox limits which host paths are visible before
the inner action starts.
In the normal mode, the outer `/tmp` is a session-scoped in-memory tmpfs reused
by action sandboxes. With `--no-sandbox`, action `/tmp` maps to the current
process's validated `TMPDIR` directly. Debug commands remain unsandboxed unless
`--with-sandbox` is supplied.

Outer filesystem mounts are applied parent-first after resolving access-rule
precedence. Explicit file and directory imports preserve symbolic links instead
of replacing them with their target's contents. Trusted and integration directory
imports inspect only their direct children for link dependencies. Ordinary child
directories remain covered by the whole-directory bind and are not traversed.
Already-visible targets reuse their effective mount without further scanning;
missing targets are added only from an already-admitted
host source, without importing a file's whole parent directory. Directory aliases
retain descendant denials and read-only restrictions. External targets outside
the admitted source scopes still require an explicit path grant.

Newly added directory targets receive the same one-level inspection. Deeper links
inside ordinary subdirectories are not proactively discovered; explicitly imported
subdirectories can be inspected independently. SSH Include discovery still follows
referenced configuration paths within the admitted view.

Directory scanning is bounded (250,000 entries, 8,192 mounts or aliases, depth
128, and 10 seconds). Exceeding a bound fails preparation; dangling links, cycles
among directory entries, and inaccessible entries retain their unavailable
behavior. Explicit cyclic mount destinations fail preparation. Runtime-provided
`/proc` and `/dev` are not recursively scanned or resolved through host process
IDs. `/usr` remains one read-only directory import, not a per-file mount tree;
base system imports and workspace/development trees are not automatic scan roots.
Inner action sandboxes inherit these system mounts
instead of repeating them; workspace overlays, read-only `.git` metadata,
reviewed grants, network isolation, and host-integration controls still apply.
Optional development paths such as `.rustup/toolchains` remain built in, but
missing sources are skipped without creating directories beneath a read-only
HOME. This applies to both inner-only and outer+inner execution.

`[permissions] network` is the inner sandbox's network capability ceiling and
defaults to `true`. It does not preauthorize network access: ordinary actions
remain network-isolated, and each action must request and obtain its own network
approval. With `network = false`, network requests are rejected before review
or execution, including in fully trusted review mode; approval cannot override
the ceiling. The setting is inherited by new process sessions and does not
restrict model-provider or configured MCP connections. Explicit `--no-sandbox`
host execution remains outside this network-isolation boundary.

Trusted `readonly_paths` and `readwrite_paths` are preauthorized in the inner
sandbox at their declared access level. `review_paths` marks existing subtrees
within these grants for **per-action** review, without adding a new grant or
raising the access ceiling. For example:

```toml
[permissions]
readonly_paths = ["~/.config"]
review_paths = ["~/.config"]
```

With `readonly_paths = ["/abc"]` and `review_paths = ["/abc/d"]`, `/abc/e` remains
readable; `/abc/d` is masked until the action explicitly requests that path or a
specific descendant through `run_process.permissions` or `request_permissions`.
Approval stays read-only and is not reused by later actions. A broader parent
grant does not unlock a separately reviewed child; nested review markers and
`deny_paths` still apply. Denied paths cannot be approved. Merry's configuration,
state directories, and configured provider credential files are product-private
and are not exposed to task processes merely because Merry itself needs them.

Enforcement uses mount namespaces, not shell-path parsing: scripts receive the
same restricted view. Directories are replaced with empty read-only mounts and
individual files with empty read-only placeholders; an empty result or missing
file inside the sandbox does not prove that the host path is absent. There is no
transparent retry or automatic elevation. Known symlink and inherited bind-mount
aliases receive the same restrictions. This is pathname isolation, not content
tracking: separately copied data and arbitrary hard-link aliases are not covered.
Missing or unprotectable review and explicit deny targets fail closed during
preflight, before any action starts. Create the configured target first or protect
an existing ancestor. Merry never creates host paths to install these masks;
optional, absent development mounts remain skippable.

`ssh_agent`, `gpg_agent`, and `dbus` independently enable outer-sandbox forwarding;
they do not preauthorize inner actions. An inner action must request the matching
host-integration capability and receive runtime approval before using the forwarded
socket or automatically imported client files. Explicit trusted path rules may
separately expose regular files, but do not approve protected agent sockets.
A missing agent does not prevent ordinary actions
from starting; explicitly requesting an unavailable endpoint reports an error. Native GPG
socket discovery uses `gpgconf --list-dirs` and honors `GNUPGHOME`; it does not
assume sockets live in `.gnupg` or `/run`. Outer scaffolding preserves private
socket-directory permissions without importing their other contents.

After approval, the SSH integration also exposes `~/.ssh/known_hosts` and
`known_hosts2` read-only to inner actions,
without granting access to private keys, `~/.ssh/config`, or the network. These
files still obey `deny_paths` and `review_paths`; an explicit deny of the entire
`.ssh` directory blocks them as well. Existing host identities can be checked,
but new or changed host keys are not automatically accepted and host trust files
are not made writable. No `StrictHostKeyChecking` or SSH configuration override
is injected.

Enabling `ssh_agent` imports `/etc/passwd` and `/etc/group` for client account
lookup, plus `/etc/ssh/ssh_config` and `/etc/ssh/ssh_config.d`, read-only. It does
not require granting the whole `/etc` directory or import SSH server
configuration, host private keys, or shadow password databases. These imports
obey explicit path denials; Include targets outside the existing sandbox view
still need an explicit `readonly_paths` grant. System SSH configuration already
exposed by filesystem rules remains available when `ssh_agent` is disabled.

Before entering a user namespace, Merry
validates regular configuration files against OpenSSH's owner/write-mode rules.
Safe root-owned files that would otherwise become UID 65534 are supplied as
unchanged, read-only private snapshots at their resolved sandbox destinations;
original symlinks remain in place and multiple aliases share one snapshot.
Ordinary mount coverage does not suppress these deliberate replacements. Snapshots travel
through sealed anonymous-memory FDs and are consumed by bubblewrap; the host
files are never modified. Snapshot mounts replace the corresponding original
file binds instead of stacking a second mount that inner actions cannot rebind.
Both the outer and inner mount plans preserve path denials and per-action review.
This does not import the whole `/etc/ssh` directory or make unsafe/unknown
ownership acceptable.

Snapshot discovery starts at `/etc/ssh/ssh_config` and follows static recursive
`Include` paths, quoted filenames, globs, and symlinks within the allowed view.
Connection-dependent percent tokens, environment/tilde expansion, and unsupported
Include patterns are left to OpenSSH and are not automatically snapshotted. This
is scoped SSH compatibility, not a global UID remapping for other programs.
Configuration copies last for the corresponding sandbox lifetime; restart the
outer sandbox to pick up host changes.

Bounded SSH discovery or read failures disable only this compatibility adaptation:
Merry reports a warning, preserves the original mounts, and lets unrelated actions
start. OpenSSH may still reject incompatible ownership; its security checks are
never disabled. Errors resolving the admitted path policy remain fatal.

Runtime owns the session capability store and retention decisions. Process adapters
consume read-only snapshots and report normalized path constraints; they do not
record grants. Each preparation captures a fresh mount-alias view, shared by path
review, masking, and client-resource discovery, rather than caching the filesystem
for an entire session. Approved host integrations may be retained within that
session; configuration flags alone never create a runtime grant. Separately
reviewed paths still require per-action approval.

`gpg_agent = true` makes the conventional public-key stores available for reviewed
use. After GPG integration approval, inner actions import `pubring.kbx` and legacy
`pubring.gpg` read-only, without additional `readonly_paths`. Each approved inner
action gets a private, temporary `GNUPGHOME` view
at the original path for locks and a fresh trust database. These client writes
never modify the host keyring, even when the host directory is declared read-only.
Host private-key files, configuration, and trust state are not automatically
imported. Listing public keys and verifying signatures do not require a running
agent; a valid signature does not imply host ownertrust was imported. This
file-based integration does not yet support keyboxd databases: their presence
is reported explicitly rather than forwarding a writable host keyboxd interface
or presenting a stale file-based keyring.

Public-key sources still obey `deny_paths` and `review_paths`. A path grant does
not authorize an otherwise masked agent socket. If a socket is also under
`review_paths`, both the integration and the exact path approval are required.
GPG's extra/browser and keyboxd endpoints are not implicitly exposed. No host
keyring is made writable as a workaround.
Direct `merry-process` consumers supply discovered `GpgAgentSockets` explicitly;
the CLI performs discovery when preparing sandboxed process backends.

`[permissions].environment` applies only inside Merry-managed action processes.
Assignments are injected after the sandbox defaults and may intentionally
override them; they do not change the outer bootstrap or provider environment.

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

Python development and verification instructions are in
[`sdks/python/README.md`](sdks/python/README.md). Architecture and contributor
contracts are in [`AGENTS.md`](AGENTS.md).
