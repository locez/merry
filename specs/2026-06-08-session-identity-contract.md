# Session Identity Contract

Date: 2026-06-08

## Purpose

Define one session identity contract before adding session store or resume.

`SessionId` is the durable identity for one Merry runtime session. It should be
safe to use in events, logs, traces, future store paths, and SDK embedding
without knowing which product entrypoint created the runtime.

The current product layers still use fixed ids such as `run`, `cmd`,
`python-sdk`, and `python-sdk-openai`. That was acceptable for early debug
slices, but it is wrong for concurrent SDK use, product logs, future persistence,
and resume. Default sessions must not collide.

## Decision

`SessionId` is an opaque runtime/session id, not a business label and not an
entrypoint label.

Default session creation should generate a random UUID v4 session id. The id
must not include entrypoint-specific prefixes such as `run-`, `cmd-`, or
`python-sdk-`.

Explicit session ids remain supported for callers that want stable identity,
future resume, or human-controlled debug handles. Explicit ids must satisfy the
same validation rules as generated ids.

## Validation

Tighten `SessionId` validation now rather than waiting for store/resume.

Accepted `SessionId` values:

- must be non-empty
- must not have leading or trailing whitespace
- must not contain control characters
- must be at most 128 characters
- must contain only ASCII letters, digits, `.`, `_`, and `-`
- must not be `.`
- must not be `..`

This makes `SessionId` filesystem-safe for the future store layout without
adding a second persisted-id type.

UUID v4 strings such as `550e8400-e29b-41d4-a716-446655440000` are valid under
this rule.

Rejected examples:

```text
""
" "
" session"
"session "
"bad/session"
"bad\\session"
"bad:session"
"bad session"
"."
".."
"bad\nsession"
```

## Default Construction

Runtime core remains explicit:

```rust
Runtime::builder(session_id)
```

The default-id helper should live where all product layers can use it. The
preferred shape is a Merry-owned helper that returns a validated `SessionId`,
for example:

```rust
SessionId::random()
```

or:

```rust
merry_core::random_session_id()
```

The exact function name can change during implementation. The important
contract is that there is one implementation of random session id generation,
not separate CLI and SDK generators with drifting formats.

The helper should use UUID v4. Add the UUID dependency at the workspace level
and keep generation inside Merry-owned code rather than requiring each caller
to format UUIDs itself.

## CLI Behavior

Product entrypoints should use a random default session id:

- `merry run` must stop using the fixed id `run`.
- `merry cmd` must stop using the fixed id `cmd`.

These commands do not need public `--session-id` flags in the first slice. They
may add one later when store/resume or user-facing debug workflows need it.

Debug and smoke commands may keep stable fixed ids when determinism is useful.
Those ids are part of debug fixture behavior, not the product default contract.

## Python SDK Behavior

Python SDK defaults should use random session ids:

```python
runtime = merry.Runtime()
runtime = merry.Runtime.from_env()
runtime = merry.Runtime.with_openai_compatible(api_key=..., model=...)
```

All three forms should create independent in-memory sessions by default.

Python constructors should also accept explicit session ids:

```python
runtime = merry.Runtime(session_id="tenant-a-debug-1")

runtime = merry.Runtime.with_openai_compatible(
    api_key=api_key,
    model=model,
    session_id="tenant-a-debug-1",
)

runtime = merry.Runtime(
    config=merry.RuntimeConfig(
        provider=provider,
        session_id="tenant-a-debug-1",
    )
)
```

`RuntimeConfig.session_id` must be honored. If it is omitted or `None`, the SDK
generates a random session id.

Invalid explicit session ids should fail at construction with the existing
SDK-facing runtime/config error shape. The rejected id value should not be
leaked in error messages.

## Store And Resume Direction

Store/resume is not part of this slice, but this contract should not block it.

Future store layout can use the session id directly as a directory name:

```text
<store_root>/<session_id>/
  manifest.json
  events.jsonl
  artifacts/
  checkpoints/
```

SDK store should be explicit opt-in. Ordinary SDK construction remains
ephemeral and in-memory by default.

Possible future API shape:

```python
runtime = merry.Runtime.from_env(store="./.merry/sessions")
runtime = merry.Runtime.resume(session_id="550e8400-e29b-41d4-a716-446655440000", store="./.merry/sessions")
```

This spec does not require that API now. It only ensures ids are already safe
for that path.

## Non-Goals

- Do not implement session persistence in this slice.
- Do not implement resume in this slice.
- Do not add a new session runner abstraction.
- Do not change runtime state ownership or the one-active-step-per-runtime
  rule.
- Do not make SDK bridge tools sandboxed by default. SDK bridge tools are
  trusted host callbacks unless the host chooses to wrap them.
- Do not require CLI users to name sessions before store/resume exists.
- Do not convert test fixture ids to random values when stable ids make tests
  clearer.

## Acceptance

Core validation:

- `SessionId::new("550e8400-e29b-41d4-a716-446655440000")` succeeds.
- `SessionId::new("tenant-a.debug_1")` succeeds.
- `SessionId::new("bad/session")` fails.
- `SessionId::new("bad session")` fails.
- `SessionId::new(".")` fails.
- `SessionId::new("..")` fails.
- Existing whitespace/control/length failures still fail.

Random generation:

- Two generated session ids are valid `SessionId` values.
- Repeated generated ids are not equal in deterministic unit coverage.
- Generated ids match UUID v4 string shape.

CLI:

- `merry run` constructs a runtime with a generated session id instead of
  `run`.
- `merry cmd` constructs a runtime with a generated session id instead of
  `cmd`.
- Debug commands that intentionally expose or accept session ids keep their
  current deterministic behavior unless a separate product decision changes it.

Python SDK:

- `merry.Runtime()` creates a native runtime with a generated session id.
- `merry.Runtime.with_openai_compatible(...)` creates a native runtime with a
  generated session id by default.
- `merry.Runtime.from_env()` passes through the same generated-id behavior.
- `RuntimeConfig.session_id` is honored when provided.
- Multiple default Python runtimes created in one process have distinct session
  ids.
- Explicit invalid Python session ids are rejected without leaking the rejected
  value.

## Implementation Notes

The first implementation should keep changes small:

1. Add one random session id helper in the owner crate.
2. Tighten `SessionId` validation and update protocol tests.
3. Replace fixed product defaults in `merry run`, `merry cmd`, and Python SDK
   OpenAI-compatible construction.
4. Keep debug/smoke/test fixture ids deterministic unless they exercise default
   product construction.
5. Add targeted tests for the new contract before broader verification.

