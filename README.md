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
- Node.js and npm when changing the local Web trajectory assets.

The embedded Web assets are checked in so a clean Rust checkout can compile
without a Node installation. Regenerate them after changing files under
`web/`:

```bash
cd web
npm ci
npm test
cd ..
cargo build --release -p merry-cli
```

`npm test` type-checks the TypeScript, runs the Web tests, and regenerates the
tracked files in `crates/merry-web/assets/`. `cargo build` embeds those files
into `target/release/merry`; it does not invoke Node or rebuild the Web assets.
CI verifies that the generated files are current.

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
policy controls workspace writes, network access, and modeled host integrations
(currently the SSH agent and D-Bus session bus). It is not a general pathname-
IPC filter: in `--inner-sandbox`, a host Unix socket that is visible through the
inherited read-only filesystem can still be reached. In the default outer+inner
mode, the outer sandbox limits which host paths are visible before the inner
action starts.
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
