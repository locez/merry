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
  on-demand details, queues, completion, resume, and a balanced magenta theme.
- Thin Rust and Python facades over the same Rust-owned runtime.

## Build

Requirements:

- Stable Rust with Rust 2024 edition support.
- Linux and `bubblewrap` for the default TUI and `merry run` sandbox.

```bash
cargo build --release -p merry-cli
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

# Generate a command plan without executing it
target/release/merry cmd "find the largest Rust files"
```

TUI and `run` use outer+inner bubblewrap automatically. `--inner-sandbox`
selects the Codex-compatible single inner sandbox, while `--no-sandbox`
selects the explicit unrestricted host mode: process actions inherit the host
filesystem, environment, and permissions without any bubblewrap namespace.
Both inner modes start from a read-only view of their parent filesystem, so
ordinary commands can see host configuration and toolchains; workspace writes,
network, and host IPC integrations remain governed by the inner action policy.
In the normal mode, the outer `/tmp` is a session-scoped in-memory tmpfs reused
by action sandboxes. With `--no-sandbox`, action `/tmp` maps to the current
process's validated `TMPDIR` directly. Debug commands remain unsandboxed unless
`--with-sandbox` is supplied.

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

Python bindings live in [`sdks/python`](sdks/python). Their primary interface
is async event iteration:

```python
runtime = merry.Runtime.with_anthropic(
    api_key="sk-ant-...",
    model="claude-sonnet-4-5",
)
stream = runtime.stream("Inspect the repository")

async for event in stream:
    print(event["type"])

result = await stream.result()
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
