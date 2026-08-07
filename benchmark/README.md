# Merry benchmark integration

This directory is the independent Harbor integration for evaluating Merry. It
is an upper-layer consumer: it has no Cargo dependency and does not become
part of the Merry runtime, provider, CLI, or SDK layers.

Harbor already publishes the SWE-Bench Verified and Terminal-Bench datasets.
This repository therefore does not copy those datasets or reimplement their
benchmark adapters. `MerryAgent` is the custom Harbor installed-agent adapter:
it uploads a Merry binary (or locates one already in the task image), invokes
the headless CLI, and leaves task verification and reward calculation to
Harbor.

## Setup

```bash
cd benchmark
uv sync
uv run ruff check .
uv run ty check
uv run pytest
```

Build the optional local binary from the repository root when the task image
does not already contain Merry:

```bash
cargo build --release -p merry-cli
```

`MERRY_BINARY_PATH` is a host-side path. The adapter uploads it to the task
container for each trial. `MERRY_CONFIG_PATH` is an optional host-side Merry
config file; it is uploaded privately and exposed through
`XDG_CONFIG_HOME`. When the config uses `api_key_file = "secrets/openai.key"`,
`MERRY_API_KEY_FILE_PATH` uploads that key separately with private permissions.
Do not put credentials in this repository.

## GitHub Actions smoke

`.github/workflows/benchmark-smoke.yml` is a manual, small-task smoke workflow.
It targets the `benchmark` GitHub environment so its credentials can be
protected from untrusted pull requests. Configure the following there, or at
repository scope:

- Secret: `MERRY_API_KEY`
- Variable: `MERRY_BASE_URL` (defaults to `https://api.openai.com/v1`)
- Variable: `MERRY_MODEL` (defaults to `gpt-4.1-mini`)
- Optional variable: `MERRY_PROTOCOL` (`responses` by default; use
  `chat_completions` for compatible vendors)
- Optional variable: `MERRY_HARBOR_MODEL` (defaults to
  `openai/<MERRY_MODEL>` for Harbor metadata)

The workflow creates the config and key file under the runner's temporary
directory, runs the existing `MerryAgent`, and removes those files in an
`always()` cleanup step. It is `workflow_dispatch`-only so model credentials
are never exposed to arbitrary pull request code.

## Run a smoke evaluation

Use a small task count before a full benchmark. Harbor's registry names below
are the current public names; inspect `harbor dataset list` before a parity run
and record the resolved dataset version in the Harbor job output.

```bash
cd benchmark
uv run harbor run \
  -d swe-bench/swe-bench-verified \
  -a merry_benchmark.agents.merry:MerryAgent \
  -m '<provider>/<pinned-model>' \
  -l 5 -n 1 \
  --agent-env MERRY_BINARY_PATH="$PWD/../target/release/merry" \
  --agent-env MERRY_CONFIG_PATH="/absolute/path/to/merry/config.toml" \
  --agent-env MERRY_API_KEY_FILE_PATH="/absolute/path/to/secrets/openai.key" \
  --agent-env MERRY_AGENT_VERSION="<git-revision>"
```

Terminal-Bench uses the same agent adapter:

```bash
uv run harbor run \
  -d terminal-bench/terminal-bench-2 \
  -a merry_benchmark.agents.merry:MerryAgent \
  -m '<provider>/<pinned-model>' \
  -l 5 -n 1 \
  --agent-env MERRY_BINARY_PATH="$PWD/../target/release/merry" \
  --agent-env MERRY_CONFIG_PATH="/absolute/path/to/merry/config.toml" \
  --agent-env MERRY_API_KEY_FILE_PATH="/absolute/path/to/secrets/openai.key" \
  --agent-env MERRY_AGENT_VERSION="<git-revision>"
```

The reference configs under `configs/` keep the dataset and conservative
concurrency settings. Run one with `uv run harbor run -c configs/<name>.yaml`
and add the same model and agent environment flags.

If Merry is already installed in the task image, omit `MERRY_BINARY_PATH` and
set `MERRY_COMMAND` to one executable token. If neither is configured, setup
fails clearly instead of silently evaluating another agent.

## Scope of adapters

Harbor's benchmark adapter converts an upstream benchmark into Harbor task
directories (`task.toml`, instruction, environment, solution, and tests). That
work belongs in Harbor's adapter and dataset repositories when a benchmark is
not already registered. Merry owns the agent adapter here. A future
Merry-specific internal suite may add a Harbor benchmark adapter without
introducing a second evaluation protocol in the runtime.
