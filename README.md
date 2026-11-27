<div align="center">

# Merry

**A lightweight terminal coding agent powered by a unified Rust runtime.**

CLI · Interactive TUI · Rust SDK · Python SDK

[Features](#features) · [Memory](#memory-footprint) · [Quick Start](#quick-start) · [Shortcuts](#tui-shortcuts) · [Sandbox](SANDBOX.md) · [SDKs](#sdks) · [Config](examples/config.toml)

</div>

https://github.com/user-attachments/assets/f7272d03-d4b8-4ae4-9ba7-547e3bc726f2

---

## Why

Honestly, there isn't much of a *why*. I just wanted an agent of my own — it sounded fun, so I built one.

---

## Features

One Rust runtime owns sessions, tool admission, context compilation, permissions, and lifecycle — serving the terminal UI, headless CLI, Rust crate, and Python SDK from the same core.

- **Lightweight Footprint** — Single native Rust binary with no JS runtime or background daemon, idling at ~27 MiB RSS in the full interactive TUI ([comparison](#memory-footprint)).
- **Streaming TUI** — Markdown & syntax-highlighted diffs, Plan Mode (`Alt+P`), inline review comments (`Alt+C`), image paste, and built-in themes (`Sakura`, `Tokyo Night`, `Catppuccin Mocha`, `Nord`, `Paper Light`, `Everforest`, `ANSI`) with optional transparency.
- **Parallel Subagents** — Built-in `explore`, `coder`, and `review` roles plus custom roles, Git worktree isolation, and live monitoring in the `Tasks` dock.
- **Sandboxed by Default** — Confines every process action via Linux Landlock + seccomp or macOS Seatbelt with workspace trust and AI/human permission review (Windows defaults to host mode with permission review and Job Object process cleanup).
- **Structured Runtime** — Automatic turn-preserving context compaction, on-demand `SKILL.md` skills, durable memory, HTTP MCP servers, and resumable sessions.
- **Multi-Provider** — OpenAI Responses & Chat Completions, Anthropic Messages, and per-role model overrides.

---

## Memory Footprint

Cold-start idle memory compared across coding agents on the same machine:

| Agent | Version | Idle RSS | Peak RSS (`VmHWM`) | vs. Merry |
| --- | --- | ---: | ---: | ---: |
| **Merry** | `0.1.0` | **26.5 MiB** | **26.5 MiB** | **1.0×** |
| Grok CLI | `1.0.89` | 114.1 MiB | 114.1 MiB | 4.3× |
| Codex CLI | `0.171.0` | 125.9 MiB | 125.9 MiB | 4.8× |
| Claude Code | `2.1.330` | 126.5 MiB | 137.2 MiB | 4.8× |
| Antigravity CLI (`agy`) | `1.5.13` | 224.9 MiB | 224.9 MiB | 8.5× |
| OpenCode CLI | `2.3.0` | 514.6 MiB | 703.7 MiB | 19.4× |
| DeepSeek Harness (server only) | `0.2.2-rc.5` | 416.5 MiB | 416.5 MiB | 15.7× |
| DeepSeek Harness (server + Chrome) | `0.2.2-rc.5` | 789.9 MiB* | — | 29.8× |

<sub>Measured on Linux `x86_64` by cold-starting each agent in a 120×40 PTY inside the same repository directory with zero prompts sent, reading total process-tree `VmRSS` / `VmHWM` from `/proc` after settling (8s). Numbers vary with OS, version, plugins, and active sessions. Claude Code was measured at its initial workspace trust screen. DeepSeek Harness (`dsh web`) excludes the `npx` wrapper; `*` adds headless Chrome's deduplicated `PSS` (334.0 MiB across all browser processes; renderer processes alone: 178.3 MiB `PSS`) to the connected `dsh` server (455.9 MiB `VmRSS`) to avoid double-counting shared browser pages.</sub>

---

## Quick Start

Supported on Linux (5.13+ for Landlock), macOS (Seatbelt), and Windows (host mode with PowerShell 7 / Windows PowerShell).

### Prebuilt binaries (no Rust or Node.js required; includes `Alt+G` Web viewer)

```bash
# Linux (static musl, x86_64 / arm64) & macOS (Apple Silicon / Intel)
curl -fsSL https://github.com/locez/merry/releases/latest/download/install.sh | sh
```

```powershell
# Windows (x86_64)
irm https://github.com/locez/merry/releases/latest/download/install.ps1 | iex
```

Or download directly from [GitHub Releases](https://github.com/locez/merry/releases), or use `cargo binstall merry-cli`.

### From crates.io (requires Rust 1.95+)

```bash
cargo install --locked merry-cli
# or with the local Web trajectory viewer (Alt+G), no Node.js required:
cargo install --locked merry-cli --features web
```

Launch Merry in any project directory — on first launch, the TUI guides you through provider setup, and settings can be adjusted anytime via `Alt+X → Settings`:

```bash
merry                                   # interactive TUI
merry resume                            # pick and resume a saved session
merry run "fix the failing tests"       # headless coding run (--events-jsonl, --resume <id>)
merry cmd "find the largest Rust files" # generate a command plan without executing
merry completions fish                  # print shell completions (bash, zsh, fish, powershell, elvish)
```

For file-based configuration (custom providers, subagent roles, MCP servers, skills, themes, and keymaps), copy [`examples/config.toml`](examples/config.toml) to `~/.config/merry/config.toml` (Linux/macOS) or `%APPDATA%\merry\config.toml` (Windows).

<details>
<summary>Optional: install from git <code>main</code></summary>

```bash
cargo install --locked --git https://github.com/locez/merry merry-cli
```

To build the local Web trajectory viewer from source, build `web/dist/` first (requires Node.js/npm):

```bash
git clone https://github.com/locez/merry.git && cd merry
(cd web && npm ci && npm run build)
cargo install --locked --path crates/merry-cli --features web
```

</details>

---

## TUI Shortcuts

Customize bindings in `[tui.keymap]` (config accepts both `alt+` and `option+`). On macOS, `Option` shortcuts below work out of the box on the standard ABC layout; enable **"Use Option as Meta Key"** in your terminal if you use a non-ABC layout or dead-key chords such as `Option+U`.

| Linux / Windows | macOS | Action |
| --- | --- | --- |
| `Alt+X` | `Option+X` | Command palette & Settings (models, themes, transparency) |
| `Alt+P` | `Option+P` | Toggle Plan Mode |
| `Alt+C` | `Option+C` | Quote timeline text and attach inline review comments |
| `Alt+T` | `Option+T` | Inspect full output of executed commands |
| `Alt+G` | `Option+G` | Open local Web trajectory viewer (`--features web`) |
| `Ctrl+V` | `Ctrl+V` / `⌘+V` | Paste image from clipboard |
| `Esc` | `Esc` | Interrupt active turn |

---

## Sandbox & Permissions

`--sandbox workspace` is on by default on Linux and macOS: process actions stay confined to the workspace with network closed unless approved, while Merry's own model and MCP connections remain on the host. On Windows, Merry currently defaults to host mode (`--sandbox off`) using `pwsh` / `powershell.exe` (overridable via `MERRY_SHELL`) with Job Object process-tree cleanup; passing `--sandbox workspace` explicitly fails closed because a native Windows kernel sandbox is not yet implemented. Permission review defaults to `--permission-mode auto` (`ask`, `always-approve`, and `deny` are also available).

See [SANDBOX.md](SANDBOX.md) for Landlock/seccomp, Seatbelt, Windows host mode, workspace trust, and host socket integrations.

---

## SDKs

### Rust ([`merry`](https://crates.io/crates/merry))

```bash
cargo add merry --features anthropic
```

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

Declare typed async tools with `#[merry::tool]`, configure context windows and compaction on providers or `AgentBuilder`, and intercept model or tool turns with `HookRegistry`. See [docs.rs/merry](https://docs.rs/merry) and [`crates/merry/src/lib.rs`](crates/merry/src/lib.rs).

### Python ([`merry-agent`](https://pypi.org/project/merry-agent/))

```bash
pip install merry-agent   # imports as `merry` (currently sdist; requires Rust 1.95+)
```

```python
from typing import Annotated
from pydantic import Field
import merry
from merry import events


@merry.tool(parallel_safe=True)
async def lookup_order(
    order_id: Annotated[str, Field(description="Order identifier.")],
) -> str:
    """Look up an order status by id."""
    return f"order {order_id}: shipped"


agent = merry.Agent(
    session_id="example",
    provider=merry.Anthropic(api_key="sk-ant-...", model="claude-sonnet-4-5"),
    system_prompt="You are a concise assistant.",
    tools=[lookup_order],
)

async with agent.stream("Check order A123.") as run:
    async for message in run:
        if isinstance(message, merry.ToolCallBatch):
            await agent.execute_tool_batch(message)
        elif isinstance(message.payload, events.AssistantMessageDeltaPayload):
            print(message.payload.delta, end="")
    result = await run.result()
```

Supports `Agent(...)` / `await Agent.resume(...)`, `@merry.tool` decorators, context window and compaction config, lifecycle hooks (`merry.Hooks`), structured outputs, and interactive sessions (`agent.interactive()`). See [`sdks/python/README.md`](sdks/python/README.md) and [`sdks/python/CAPABILITY_MATRIX.md`](sdks/python/CAPABILITY_MATRIX.md).

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
