# Observability-First Coding Loop Design

Date: 2026-05-23

## Purpose

Merry has proven that deterministic, sandboxed, and live-provider tool calls can
complete the minimal coding loop. The next milestone should make that loop
observable before adding another interaction surface. The immediate problem is
not the absence of an event CLI. It is that key runtime, tool, process,
provider, sandbox, and artifact actions are not consistently logged in a way a
human operator can follow while testing real behavior.

The chosen direction is configuration-backed observability-first: add an XDG
and TOML based configuration system, then use it to control structured
logging/tracing at the action boundaries that already matter. Runtime events
remain protocol evidence. A future CLI/TUI can render those events and logs,
but the next milestone should first answer: what did Merry do, why did it do
it, what did the model request, what did each tool run, what artifact was
recorded, and where did the loop stop?

This is also the prerequisite for useful multi-turn testing. A prompt loop or
REPL without logs would still be opaque when the model chooses a surprising
tool, loses task context, hits policy, or records an artifact the operator
cannot inspect.

## Current Evidence

The current runtime already records protocol evidence:

- `Runtime::step` emits runtime events such as `SessionStarted`,
  `StepStarted`, `ToolCallPending`, `ArtifactRecorded`,
  `ToolCallResolved`, `StepCompleted`, `Failed`, and `Cancelled`.
- `Runtime::run_agent_loop` composes step -> tool execution -> continuation
  until completion, cancellation, failure, or a bounded blocked state.
- Tool output is recorded as an artifact before `ToolCallResolved` is emitted.
- Ledger facts are recorded alongside observable events so state is written
  before the event claims it.
- `merry shell --events-jsonl` can already print some raw runtime event streams.
- `merry --with-sandbox debug coding-loop-live-smoke` proves the live loop, but
  currently reports only `coding-loop-live-smoke: ok` on success.
- `merry-provider-openai` already uses local `tracing` spans for parts of the
  HTTP/streaming path.
- Current live-smoke configuration is CLI/debug specific and reads a repo-local
  `.merry/secrets/openai.env` file. That path helped prove the smoke, but it is
  not the long-term configuration boundary.

The gap is broader than event rendering:

- runtime loop start/stop and step boundaries are not systematically logged
- tool admission, execution start/end, and result status are not visible as a
  coherent trace
- process argv/cwd/exit/output-size evidence exists in artifacts, but the
  operator does not see a concise action log while a smoke runs
- live smoke success hides the model/tool path behind `ok`
- failures can require source inspection instead of reading one correlated log
  stream
- future multi-turn testing needs stable `turn_index`, `step_index`, and
  `tool_call_id` style correlation before a UI is useful
- global settings, provider settings, model selection, and observability
  settings need one durable config source instead of one-off command flags or
  repo-local smoke files

## User Experience

Add default configuration discovery under the XDG base directory rules. Merry
should read:

```text
$XDG_CONFIG_HOME/merry/config.toml
fallback: ~/.config/merry/config.toml
```

Merry should also be prepared to use XDG state paths for logs:

```text
$XDG_STATE_HOME/merry/logs/merry.jsonl
fallback: ~/.local/state/merry/logs/merry.jsonl
```

The first config file can be small but should establish the future shape:

```toml
[global]
profile = "default"

[observability.log]
enabled = false
level = "info"
format = "json"
# Optional. If omitted and logging is enabled, use the XDG state fallback.
path = "~/.local/state/merry/logs/merry.jsonl"

[providers.default]
provider = "openai-compatible"
model = "gpt-4.1-mini"

[providers.openai-compatible]
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
```

Logging must be off by default. Enabling logs, choosing level/format, and
choosing the output path should be configuration decisions, not new root command
line flags such as `--log-level` or `--log-format`.

The existing debug smoke command shape should remain stable:

```bash
merry --with-sandbox debug coding-loop-smoke
```

The live path should also keep its command shape stable:

```bash
merry --with-sandbox debug coding-loop-live-smoke
```

When `--with-sandbox` is used, the sandbox bootstrap should make the resolved
Merry config directory available inside the sandbox. For v1 this should be a
read-only mount of the host Merry config directory into the sandbox user's XDG
config location, such as `/home/merry/.config/merry`, with `XDG_CONFIG_HOME`
set consistently inside the sandbox. If file logging is enabled, the configured
or default XDG state/log directory should be mounted read-write only for that
logging path.

Logs should not replace command stdout. Existing command output, such as
`coding-loop-live-smoke: ok`, should remain on stdout so scripts and tests can
continue to distinguish command result from diagnostics. If logging is enabled
and no file path is configured, logs should go to
`$XDG_STATE_HOME/merry/logs/merry.jsonl`, falling back to
`~/.local/state/merry/logs/merry.jsonl`. Merry should create the parent
directory when possible. If the directory cannot be created or the log file
cannot be opened, the command should fail with a clear diagnostic instead of
silently falling back to stderr.

A readable text log mode is useful for manual testing, but JSON logs should be
the primary stable shape for regression capture. A representative JSON log
sequence should contain records like:

```text
runtime.loop.start session_id=... max_steps=10
runtime.step.start session_id=... step_index=1
runtime.provider.request model=... tool_count=3 continuation_count=0
runtime.tool.pending tool_call_id=... tool_name=run_process
runtime.tool.execute.start tool_call_id=... tool_name=run_process
runtime.process.execute.start argv=["rg","--files"] cwd=.merry/local/...
runtime.process.execute.finish exit_status=0 stdout_bytes=...
runtime.artifact.record artifact_id=... artifact_kind=json
runtime.tool.execute.finish tool_call_id=... status=succeeded artifact_id=...
runtime.loop.finish status=completed steps_run=...
```

Exact `RuntimeEvent` JSONL remains valuable protocol output, but it is no longer
the milestone center. Events answer "what state changed"; logs must answer
"what action is happening now and what context helps diagnose it".

## Scope

In scope:

- XDG config discovery for Merry's config directory and `config.toml`.
- TOML config parsing for global settings, observability logging, default model,
  and OpenAI-compatible provider settings.
- Migration path from repo-local `.merry/secrets/openai.env` live-smoke config
  to user-local XDG TOML config.
- `--with-sandbox` mounting of the resolved Merry config directory into the
  sandbox, read-only by default.
- Optional XDG state/log directory mounting when file logging is enabled.
- CLI-owned tracing subscriber setup driven by config, not by new logging
  command-line flags.
- Runtime `tracing` spans/events for agent loop, step, provider boundary,
  tool-pending, tool-execution, artifact-recording, failure, cancellation, and
  loop terminal status.
- Process actuator logs for intent/admission, exact argv, cwd, exit status,
  stdout/stderr byte counts, and result artifact ID.
- Workspace tool logs for read/list/search/patch action start/end, target path,
  status, and artifact ID where applicable.
- OpenAI-compatible provider logs aligned with the runtime correlation fields,
  without leaking provider wire payloads into runtime.
- Sandbox/live smoke logs that show the full path behind `ok`.
- Stable correlation fields: session ID, loop run ID if available, step index,
  tool call ID, tool name, artifact ID, status, and diagnostic code.
- Redaction rules for secrets and bounded previews.
- Deterministic tests that capture tracing output without network or bwrap.
- Existing opt-in live/sandbox smoke remains non-default.

Out of scope:

- New root logging flags such as `--log-level` or `--log-format`.
- Full-screen TUI.
- General coding agent behavior.
- A new REPL or multi-turn prompt UI in this milestone.
- Arbitrary shell strings, pipelines, inherited env, stdin, network tools, or
  broad filesystem writes.
- A new approval system.
- Runtime/provider conversation state.
- Making live provider behavior part of default tests.

## Architecture

The first implementation should keep log collection/display at the CLI edge
while adding instrumentation to the library crates where the actions happen.
Runtime code should not own terminal formatting. CLI code should not infer
runtime state by parsing event JSON.

Recommended components:

- `MerryConfig`: validated TOML-backed config model for global, provider/model,
  and observability settings.
- `XdgPaths`: path resolver for `$XDG_CONFIG_HOME`, `$XDG_STATE_HOME`, and their
  default fallbacks. Relative XDG env values should be treated as invalid and
  ignored, following the XDG base directory rules.
- `ObservabilityConfig`: config-derived log enablement, level, format, and
  destination.
- CLI subscriber setup using `tracing-subscriber`, kept in `merry-cli`, and
  initialized only when config enables logging.
- Sandbox config mount planning that resolves host config/state paths before
  bwrap re-exec and exposes them consistently inside the sandbox.
- Runtime instrumentation using `tracing` spans/events only; no dependency on
  CLI formatting or subscriber implementation.
- Small helper functions for stable, safe summaries of tool arguments,
  artifact refs, process evidence, and diagnostics.
- Optional test-only tracing capture helpers for deterministic assertions.

The runtime should log artifact IDs and bounded metadata, not full artifact
contents by default. Exact evidence remains available through artifacts and
runtime events.

## Logging Contract

The MVP log contract should cover these action points:

- `runtime.loop.start`: session ID, max steps, generation controls that are
  safe to expose.
- `runtime.loop.finish`: status, steps run, blocked reason or diagnostic code.
- `runtime.step.start`: session ID, step index, whether this is continuation.
- `runtime.step.finish`: completed/failed/cancelled/pending outcome.
- `runtime.provider.request`: provider name, model, message count, tool count,
  continuation count, generation controls.
- `runtime.provider.stream.finish`: completed/cancelled/error status and
  provider-neutral error kind when available.
- `runtime.tool.pending`: tool call ID, tool name, safe argument summary.
- `runtime.tool.execute.start`: tool call ID and tool name.
- `runtime.tool.execute.finish`: status, artifact ID, diagnostic code where
  applicable.
- `runtime.artifact.record`: artifact ID, artifact kind, source action, byte
  count when cheaply available.
- `runtime.process.intent`: argv, cwd, intent classification, admission profile.
- `runtime.process.execute.start`: argv, cwd, timeout/output limits when known.
- `runtime.process.execute.finish`: exit status, stdout/stderr bytes, artifact
  ID, cancellation/error status.
- `runtime.workspace_tool.start/finish`: tool name, path or query summary,
  status, artifact ID.

Field names should be boring and stable. Do not log secrets, API keys, raw
provider requests, full prompts, full model output, full file contents, or full
stdout/stderr by default.

## Configuration Contract

The MVP config contract should be intentionally small and extensible:

- read `$XDG_CONFIG_HOME/merry/config.toml`, falling back to
  `~/.config/merry/config.toml`
- ignore relative XDG env values and fall back to defaults
- keep missing config non-fatal for commands that do not need config
- fail with a clear diagnostic when a command requires provider/model config
  and the config is missing or invalid
- support global defaults separately from provider-specific settings
- support observability logging config with `enabled`, `level`, `format`, and
  optional `path`
- when logging is enabled and `path` is omitted, write to
  `$XDG_STATE_HOME/merry/logs/merry.jsonl`, falling back to
  `~/.local/state/merry/logs/merry.jsonl`
- create the default log directory when possible and fail clearly if the log
  path cannot be opened
- keep API keys out of logs; prefer `api_key_env` for the first version, while
  leaving room for future local secret file support under the Merry config
  directory
- do not read repo-local `.merry/secrets/openai.env` as the long-term default
  once XDG config support exists

The config parser should use a real TOML parser, not ad hoc `KEY=value`
parsing. TOML schema tests should cover missing optional sections, invalid log
levels/formats, invalid provider config, and redaction of secret-like fields in
diagnostics.

Reference:

- XDG Base Directory Specification:
  https://specifications.freedesktop.org/basedir-spec/latest/

## Error Handling

The logs should make failure boundaries obvious:

- `--with-sandbox` evidence is missing.
- XDG Merry config is missing when required, invalid TOML, or semantically
  invalid.
- sandbox config directory mount planning fails.
- the model emits multiple pending tool calls.
- the model emits completion and a pending call in the same step.
- the loop reaches max steps.
- provider setup, transport, stream parse, or cancellation fails.
- process admission or execution fails.
- workspace read/search/patch policy rejects an operation.

Command-level usage errors should remain direct stderr messages. Runtime and
tool actions should produce structured logs before returning errors where doing
so does not violate state-before-event rules.

## Testing

Default tests must stay deterministic and offline.

Test coverage should include:

- XDG path resolution for set/unset/empty/relative `$XDG_CONFIG_HOME` and
  `$XDG_STATE_HOME`.
- TOML config parsing for global, observability, provider, and model settings.
- Default log path tests for omitted `observability.log.path`.
- Log path failure tests for parent directory creation and file open errors.
- Sandbox plan tests proving `--with-sandbox` mounts the Merry config directory
  read-only and mounts the configured/default log directory only when needed.
- CLI log configuration and subscriber setup smoke tests driven by config.
- Runtime agent-loop tracing capture for start, step, tool pending, tool
  execution, artifact recording, and terminal status using fake provider/fake
  runner.
- Process tool tracing capture for admitted execution, denied command, failure,
  cancellation, and output byte counts using fake runner.
- Provider tracing tests that assert safe metadata is logged without raw API
  keys or provider wire payloads.
- Redaction tests for config/env fields and bounded summaries.
- Existing event/ledger/artifact ordering tests remain authoritative for state
  correctness.

Opt-in checks:

- Existing deterministic bwrap smoke remains ignored by default.
- Existing live smoke remains ignored/manual.
- A manual log-enabled live smoke should demonstrate that the operator can see
  model request metadata, tool choices, process execution, artifact IDs, and
  final status.

## Multi-Turn Follow-Up

Multi-turn testing is important, but it should follow the observability slice.
The next interactive surface should be able to reuse the same log correlation
fields and should not need to rediscover what the runtime did by scraping
stdout.

A later interactive command or REPL should support:

- multiple user turns in one runtime session
- visible logs per turn and step
- artifact IDs that can be inspected after each turn
- clear separation between user input, model output, logs, and protocol events

TUI remains technically viable with Ratatui and Crossterm, but it should be
considered only after logs show which persistent panes or interactive controls
are actually useful.

- Ratatui documentation: https://ratatui.rs/
- Crossterm crate documentation: https://docs.rs/crossterm/latest/crossterm/

## Acceptance Criteria

- A user can run a sandboxed deterministic coding-loop smoke with logging
  enabled through `~/.config/merry/config.toml` and see the runtime loop,
  provider boundary, tool choices, process execution, artifact IDs, and final
  status in one correlated log stream.
- If `observability.log.path` is omitted, logs are written to
  `$XDG_STATE_HOME/merry/logs/merry.jsonl`, falling back to
  `~/.local/state/merry/logs/merry.jsonl`.
- If that default log file cannot be opened, Merry reports a clear error and
  does not silently fall back to stderr.
- `merry --with-sandbox` mounts the resolved Merry config directory into the
  sandbox so deterministic and live smokes can read the same user-local config.
- A user can run the live coding-loop smoke with config-backed logging and see
  why the model/tool loop succeeded, failed, blocked, or cancelled instead of
  only seeing `ok`.
- Logs include stable correlation fields for session ID, step index, tool call
  ID, tool name, artifact ID, status, and diagnostic code where applicable.
- Logs do not expose API keys, local secret file contents, full prompts, full
  provider payloads, or unbounded stdout/stderr/file contents by default.
- Default tests do not require bwrap, network, or live credentials.
- The design does not introduce a full TUI, a new REPL, or general coding-agent
  scope in this milestone.
