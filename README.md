# Merry

A sandboxed terminal coding agent powered by a unified Rust runtime, with CLI/TUI, Rust, and Python APIs.

One Rust runtime owns session state, tool admission, context compilation, checkpoints, permissions, and lifecycle — serving the interactive terminal UI, headless CLI, Rust facade, and Python SDK from the same foundation.

https://github.com/user-attachments/assets/f7272d03-d4b8-4ae4-9ba7-547e3bc726f2

*Interactive TUI · Parallel Subagents (`Tasks` dock) · Syntax-Highlighted Diff · Inline Review Comments (`Alt+C`)*

---

## Highlights

- **Interactive Streaming TUI** — Markdown and syntax-highlighted diff rendering, Plan Mode (`Alt+P`), inline quote & review comments (`Alt+C`), full command output inspector (`Alt+T`), clipboard image paste (`Ctrl+V`), command palette (`Alt+X`), customizable themes and keymaps, and a Tasks dock with live subagent takeover.
- **Multi-Role Subagents & Worktree Isolation** — Built-in `explore`, `coder`, and `review` roles plus custom roles in `config.toml` or `.merry/roles/<name>.toml`. Concurrent editing tasks can run in isolated Git worktrees with independent model, reasoning effort, turn, and timeout budgets.
- **Workspace Sandbox & Permission Review** — Confines every process action by default (`--sandbox workspace`) using Linux **Landlock + seccomp** or macOS **Seatbelt**, while keeping Merry's own model and MCP connections on the host. Includes first-open workspace trust prompts (`read-write` / `read-only`) and configurable permission review (`auto`, `ask`, `always-approve`, or `deny`).
- **Structured Context, Skills, Memory & MCP** — Deterministic context compilation with automatic turn-preserving compaction, on-demand `SKILL.md` loading, durable cross-session memory, HTTP MCP server integration (with frozen session schemas and offline reconnection), parallel tool execution, and resumable sessions.
- **Multi-Provider & Per-Role Models** — Supports OpenAI Responses API, OpenAI Chat Completions (`openai-compatible`), and Anthropic Messages (`anthropic`), with independent model overrides for `context_compaction`, `approval_review`, and subagent roles.
- **Optional Web Trajectory Viewer** — Inspect full session trajectories locally in your browser (`Alt+G`) when built with `--features web`.
- **Rust & Python SDKs** — Embed the exact same Rust-owned runtime and typed tool contracts directly in Rust (`merry`) or async Python (`sdks/python`).

---

## Quick Start

### Prerequisites

- **Rust 1.95+**
- **Sandbox support**: Linux 5.13+ (Landlock) or macOS (Seatbelt). On kernels without Landlock, pass `--sandbox off`.
- **Optional Web trajectory UI**: Node.js / npm (only required when building with `--features web`).

### Install

Install the `merry` binary with Cargo:

```bash
cargo install --locked --git https://github.com/locez/merry merry-cli
```

To install with the optional local Web trajectory viewer (`Alt+G`), build the frontend assets under `web/dist/` from a local checkout first:

```bash
git clone https://github.com/locez/merry.git
cd merry
(cd web && npm ci && npm run build)
cargo install --locked --path crates/merry-cli --features web
```

> Cargo does not invoke `npm`. With `--features web`, a missing asset under `web/dist/` stops installation and reports the missing path.

### Configure

Copy [`examples/config.toml`](examples/config.toml) to `$XDG_CONFIG_HOME/merry/config.toml` (or `~/.config/merry/config.toml`). Store API keys in separate files and point `api_key_file` at them:

```toml
[providers.default]
provider = "openai-compatible"
model = "gpt-4.1-mini"

[providers.openai-compatible]
type = "openai-compatible"
protocol = "responses" # or "chat_completions"
base_url = "https://api.openai.com/v1"
api_key_file = "secrets/openai.key"
```

See [`examples/config.toml`](examples/config.toml) for Anthropic Messages, per-role overrides (`[models.context_compaction]`, `[models.approval_review]`), subagent roles, skills, MCP servers, TUI themes, and keymaps.

### Run

```bash
merry                                 # launch interactive TUI
merry resume                          # pick and resume a saved TUI session
merry run "fix the failing tests"     # run a headless coding task
merry cmd "find the largest Rust files" # generate a command plan (no execution)
```

---

## CLI & Interactive TUI

### CLI Commands

```bash
merry                                      # streaming TUI
merry resume                               # browse and resume a saved session
merry run "fix the failing tests"          # headless run (reads stdin when task is "-")
merry run --events-jsonl "inspect failure" # stream structured JSONL runtime events
merry run --resume <session-id> "continue" # continue a saved session
merry cmd "list recently modified crates"  # produce a shell command plan without running it
merry completions fish                     # print shell completions (no config or sandbox needed)
```

Sessions are persisted under `$XDG_STATE_HOME/merry/sessions/<session-id>`. Pass `--session-id <id>` to choose an explicit id (existing ids are rejected to prevent accidental overwrites).

### Essential TUI Shortcuts

On macOS, `Alt` maps to `Option`. All shortcuts except `Alt+X` can be customized under `[tui.keymap]` in `config.toml`.

| Shortcut | Action |
| --- | --- |
| `Alt+X` | Open command palette |
| `Alt+P` | Toggle Plan Mode |
| `Alt+C` | Quote timeline text and attach review comments |
| `Alt+T` | Inspect full captured output of executed commands |
| `Alt+U` | Review previous user input |
| `Alt+G` | Open local Web trajectory viewer (requires `--features web`) |
| `Ctrl+V` | Paste image from clipboard |
| `Ctrl+R` | Search input history |
| `Esc` | Interrupt active turn |
| `Ctrl+Q` | Quit |

---

## Sandbox & Permissions

`--sandbox workspace` is enabled by default. Every process action runs confined to the workspace and an explicit baseline allow-list, with network disabled unless a request asks for and receives approval.

- **First-open workspace trust**: Opening a directory in workspace mode prompts whether commands may edit files (`read-write`) or only inspect them (`read-only`). Inside a Git repository, trust is recorded for the repo root in `$XDG_STATE_HOME/merry/workspace-trust.json`. In non-interactive runs, pass `--trust-workspace yes|no`.
- **Permission modes** (`--permission-mode`):
  - `auto` *(default)* — an approval-review model evaluates requests first, falling back via `--auto-fallback ask|deny` (default: `ask`).
  - `ask` — prompt the user directly.
  - `always-approve` — skip permission review (also available as `--always-approve`).
  - `deny` — reject requests and return a failed tool result to the model without terminating the turn.
- **Host execution**: `--sandbox off` runs commands on the host without dropping permission review.

See [SANDBOX.md](SANDBOX.md) for Linux Landlock/seccomp details, macOS Seatbelt behavior, allowed paths, network controls, and optional `ssh_agent` / `gpg_agent` / `dbus` integrations.

---

## Subagents, Skills & MCP

- **Subagents**: Configured under `[subagents]` in `config.toml`. Built-in `explore`, `coder`, and `review` roles can each override provider, model, reasoning effort, turn limit, and timeout. Custom roles can live in `config.toml` or `.merry/roles/<name>.toml`.
- **Skills**: Point `[skills].roots` at directories containing `SKILL.md` skills. Metadata stays in the cacheable request prefix while full skill bodies are read on demand.
- **MCP**: Register trusted HTTP MCP servers under `[mcp.<name>]`. Connection failures warn at startup without blocking Merry, session tool schemas stay frozen across resume, and offline servers reconnect automatically on use.

---

## Embedding Merry

### Rust SDK (`merry`)

The `merry` crate exposes the typed facade, provider builders, event streams, structured output contracts, and `#[merry::tool]` macro over the same runtime used by the CLI:

```rust
use merry::{AgentBuilder, SessionId, providers::anthropic};

let provider = anthropic()
    .api_key("sk-ant-...")?
    .model_name("claude-sonnet-4-5")?
    .build()?;

let agent = AgentBuilder::new(SessionId::new("example")?)
    .provider(provider)
    .build()?;

let result = agent
    .run("Inspect the repository and summarize the architecture.")
    .await?;
```

Annotate typed async functions with `#[merry::tool]` to generate tool factories (e.g., `greet_tool()`) and register them explicitly on `AgentBuilder::tool(...)`. The macro does not register global state or alter runtime admission policy.

### Python SDK (`sdks/python`)

The PyO3-backed Python SDK wraps the same Rust-owned session lifecycle with strongly typed Pydantic models and async event streaming:

```python
import merry

agent = (
    merry.Agent.builder("example")
    .provider(merry.Anthropic(api_key="sk-ant-...", model="claude-sonnet-4-5"))
    .build()
)

run = agent.stream("Inspect the repository")
async for message in run:
    if isinstance(message, merry.Event):
        print(message.type.value)

result = await run.result()
```

See [`sdks/python/README.md`](sdks/python/README.md) for installation (`uv sync` / `uv build`), `@builder.tool`, structured final output, and examples, and [`sdks/python/CAPABILITY_MATRIX.md`](sdks/python/CAPABILITY_MATRIX.md) for Rust/Python parity.

---

## Architecture

Merry is organized as a layered Cargo workspace where dependencies flow strictly from product surfaces and adapters toward provider-neutral contracts and runtime services:

| Crate | Responsibility |
| --- | --- |
| `merry-core` | Shared IDs, errors, schemas, artifacts, and journal/event contracts |
| `merry-llm` | Provider traits, normalized requests/responses/events, and capabilities |
| `merry-runtime` | Sessions, task ledger, context compilation, compaction, memory, permissions, and execution |
| `merry-process` | Host-process boundary and Landlock/seccomp/Seatbelt sandbox runners |
| `merry-provider-openai` / `merry-provider-anthropic` | External LLM protocol adapters (wire formats stay private) |
| `merry-tools` / `merry-tools-macros` / `merry-mcp` | Workspace tools, `#[tool]` proc-macro, and HTTP MCP adapter |
| `merry-coding` | Coding profile: prompts, project rules, tool catalog, and permission policy |
| `merry` | Public Rust facade (`AgentBuilder`, providers, profiles, typed tools) |
| `merry-cli` / `merry-web` | Terminal product surface (CLI/TUI) and optional local Web trajectory viewer |
| `merry-py` + `sdks/python` | Thin PyO3 bridge and typed async Python SDK |

---

## Development & Verification

```bash
# Rust checks
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all

# Optional Web trajectory UI checks
(cd web && npm ci && npm test)
cargo test -p merry-cli --features web

# Python SDK checks
(cd sdks/python && uv sync && uv run --with ruff --with ty ruff check . && uv run --with ruff --with ty ty check && uv run --with pytest python -m pytest tests -q)
```

---

## Documentation

- [SANDBOX.md](SANDBOX.md) — Workspace sandbox, allowed paths, network isolation, and permission modes.
- [AGENTS.md](AGENTS.md) — Architecture boundaries, engineering standards, and contributor contracts.
- [`examples/config.toml`](examples/config.toml) — Fully annotated configuration reference.
- [`sdks/python/README.md`](sdks/python/README.md) — Python SDK guide and examples.
- [`sdks/python/CAPABILITY_MATRIX.md`](sdks/python/CAPABILITY_MATRIX.md) — Rust and Python SDK parity.

---

## License

Merry is dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Merry by you, as defined in the Apache-2.0 license, shall be dual-licensed as above, without any additional terms or conditions.
