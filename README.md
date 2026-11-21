<div align="center">

# Merry

**A sandboxed terminal coding agent powered by a unified Rust runtime.**

CLI · Interactive TUI · Rust SDK · Python SDK

[Quick Start](#quick-start) · [Shortcuts](#tui-shortcuts) · [Sandbox](SANDBOX.md) · [SDKs](#sdks) · [Config](examples/config.toml)

</div>

https://github.com/user-attachments/assets/f7272d03-d4b8-4ae4-9ba7-547e3bc726f2

---

## Why Merry

One Rust runtime owns sessions, tool admission, context compilation, permissions, and lifecycle — serving the terminal UI, headless CLI, Rust crate, and Python SDK from the same core.

- **Streaming TUI** — Markdown & syntax-highlighted diffs, Plan Mode (`Alt+P`), inline review comments (`Alt+C`), image paste, and built-in themes (`Sakura`, `Tokyo Night`, `Catppuccin Mocha`, `Nord`, `Paper Light`, `ANSI`) with optional transparency.
- **Parallel Subagents** — Built-in `explore`, `coder`, and `review` roles plus custom roles, Git worktree isolation, and live monitoring in the `Tasks` dock.
- **Sandboxed by Default** — Confines every process action via Linux Landlock + seccomp or macOS Seatbelt with workspace trust and AI/human permission review.
- **Structured Runtime** — Automatic turn-preserving context compaction, on-demand `SKILL.md` skills, durable memory, HTTP MCP servers, and resumable sessions.
- **Multi-Provider** — OpenAI Responses & Chat Completions, Anthropic Messages, and per-role model overrides.

---

## Quick Start

Requires **Rust 1.95+** (and Linux 5.13+ for Landlock or macOS for Seatbelt; pass `--sandbox off` otherwise).

```bash
cargo install --locked --git https://github.com/locez/merry merry-cli
```

Launch Merry in any project directory — on first launch, the TUI guides you through provider setup, and settings can be adjusted anytime via `Alt+X → Settings`:

```bash
merry                                   # interactive TUI
merry resume                            # pick and resume a saved session
merry run "fix the failing tests"       # headless coding run (--events-jsonl, --resume <id>)
merry cmd "find the largest Rust files" # generate a command plan without executing
merry completions fish                  # print shell completions
```

For file-based configuration (custom providers, subagent roles, MCP servers, skills, themes, and keymaps), copy [`examples/config.toml`](examples/config.toml) to `~/.config/merry/config.toml`.

<details>
<summary>Optional: install with the local Web trajectory viewer (<code>Alt+G</code>)</summary>

Requires Node.js/npm to build the frontend assets under `web/dist/` first:

```bash
git clone https://github.com/locez/merry.git && cd merry
(cd web && npm ci && npm run build)
cargo install --locked --path crates/merry-cli --features web
```

</details>

---

## TUI Shortcuts

On macOS, `Alt` maps to `Option`. Customize bindings in `[tui.keymap]`.

| Key | Action |
| --- | --- |
| `Alt+X` | Command palette & Settings (models, themes, transparency) |
| `Alt+P` | Toggle Plan Mode |
| `Alt+C` | Quote timeline text and attach inline review comments |
| `Alt+T` | Inspect full output of executed commands |
| `Alt+G` | Open local Web trajectory viewer (`--features web`) |
| `Ctrl+V` | Paste image from clipboard |
| `Esc` | Interrupt active turn |

---

## Sandbox & Permissions

`--sandbox workspace` is on by default: process actions stay confined to the workspace with network closed unless approved, while Merry's own model and MCP connections remain on the host. Permission review defaults to `--permission-mode auto` (`ask`, `always-approve`, and `deny` are also available).

See [SANDBOX.md](SANDBOX.md) for Landlock/seccomp, Seatbelt, workspace trust, and host socket integrations.

---

## SDKs

### Rust (`merry`)

```rust
use merry::{AgentBuilder, SessionId, providers::anthropic};

let provider = anthropic()
    .api_key("sk-ant-...")?
    .model_name("claude-sonnet-4-5")?
    .build()?;

let agent = AgentBuilder::new(SessionId::new("example")?)
    .provider(provider)
    .build()?;

let result = agent.run("Inspect the repository and summarize key modules.").await?;
```

Declare typed async tools with `#[merry::tool]` and register them explicitly via `AgentBuilder::tool(...)`.

### Python (`sdks/python`)

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

See [`sdks/python/README.md`](sdks/python/README.md) and [`sdks/python/CAPABILITY_MATRIX.md`](sdks/python/CAPABILITY_MATRIX.md).

---

## Docs & Development

- [`examples/config.toml`](examples/config.toml) — annotated configuration reference
- [SANDBOX.md](SANDBOX.md) — sandbox isolation and permission model
- [AGENTS.md](AGENTS.md) — workspace architecture, crate boundaries, and contributor standards

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

---

## License

Dual-licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT), at your option.

Unless explicitly stated otherwise, any contribution intentionally submitted for inclusion in Merry, as defined in the Apache-2.0 license, is dual-licensed as above without additional terms or conditions.
