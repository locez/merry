# Merry

Merry is a terminal coding agent. One Rust runtime owns the session and also serves the Rust and Python APIs.

## Status

Merry is in active development and has no release yet. The runtime, CLI, TUI, Rust facade, and Python SDK run end to end. The open milestone is release gates. [ROADMAP.md](ROADMAP.md) has the sequence.

## Quick start

Rust 1.95 or newer. The default sandbox needs Linux 5.13 or newer for Landlock; otherwise pass `--sandbox off`.

```bash
cargo build --release -p merry-cli
```

The local Web trajectory view (`Alt+G`) is optional and requires Node.js/npm to build its frontend assets under `web/dist/` before compiling with `--features web`:

```bash
cd web && npm ci && npm test && cd ..
cargo build --release -p merry-cli --features web
```

Cargo does not run npm. With `--features web`, a missing file under `web/dist/` stops the build and names that file.

Copy [`examples/config.toml`](examples/config.toml) to `$XDG_CONFIG_HOME/merry/config.toml`, or to `~/.config/merry/config.toml`. Keep the API key in a separate file and point `api_key_file` at it.

```bash
target/release/merry
target/release/merry run "fix the failing tests"
```

## Configure

One provider is enough. This selects the OpenAI Responses API:

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

The example file also shows Chat Completions, Anthropic Messages, and per-role models such as `context_compaction` and `approval_review`. Configured HTTP MCP servers are optional: a connection failure warns, and Merry still starts. Invalid config fails validation.

## CLI

```bash
target/release/merry                         # streaming TUI
target/release/merry resume                  # pick a saved TUI session
target/release/merry run "fix the failing tests"
target/release/merry run --events-jsonl "inspect the current failure"
target/release/merry cmd "find the largest Rust files"  # a plan, not an execution
target/release/merry completions fish        # print a completion script
```

`merry run` saves the session under `$XDG_STATE_HOME/merry/sessions/<session-id>`. Continue that session with `--resume <session-id>`. `--session-id` starts a new session and refuses an id that already exists. A task of `-` reads the prompt from stdin.

`merry completions` prints the script and exits. It does not read config or start a sandbox.

## Sandbox

`--sandbox workspace` is the default. Each process action is confined. Merry's own model and MCP connections stay on the host. The first time a directory is opened, a dialog asks whether commands may edit files there. Read-only leaves the directory read-only. Either choice can be saved for later.

`--sandbox off` runs actions on the host and does not skip permission review. `--permission-mode` chooses the reviewer: `auto` (default), `ask`, `always-approve`, or `deny`. A denial is a failed tool result. It does not end the turn by itself.

Paths, network, and host integrations are documented in [SANDBOX.md](SANDBOX.md).

## Library

The `merry` crate is the Rust facade. A provider builder returns the same provider-neutral component the CLI uses:

```rust
use merry::{Runtime, SessionId, providers::{RuntimeBuilderProviderExt, anthropic}};

let provider = anthropic()
    .api_key("sk-ant-...")?
    .model_name("claude-sonnet-4-5")?
    .build()?;
let runtime = Runtime::builder(SessionId::new("example")?)
    .with_provider(provider)
    .build()?;
```

`#[merry::tool]` on a typed async function generates a factory, such as `greet_tool()`. Register that factory on `merry::AgentBuilder`. The macro does not register tools globally and does not change execution policy.

Python in [`sdks/python`](sdks/python) wraps the same Rust-owned lifecycle:

```python
agent = (
    merry.Agent.builder("example")
    .provider(merry.Anthropic(api_key="sk-ant...", model="claude-sonnet-4-5"))
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

CI runs those Rust checks, the Web contract check, and the Python SDK workflow.

## Documentation

- [ROADMAP.md](ROADMAP.md) — delivery sequence and current focus.
- [SANDBOX.md](SANDBOX.md) — sandbox and permission reference.
- [AGENTS.md](AGENTS.md) — crate ownership and contributor contracts.
- [`examples/config.toml`](examples/config.toml) — annotated configuration.
- [`sdks/python/README.md`](sdks/python/README.md) — Python development.
- [`sdks/python/CAPABILITY_MATRIX.md`](sdks/python/CAPABILITY_MATRIX.md) — Rust and Python parity.
