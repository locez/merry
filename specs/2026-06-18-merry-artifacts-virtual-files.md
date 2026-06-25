# `merry_artifacts://` Virtual Artifact Files

Status: Draft

## Purpose

Merry already treats artifacts as durable, exact runtime evidence that should
not be projected into prompt context by default. The missing surface is a
stable way for tools, especially shell/process tools, to consume artifact
content without copying large blobs into model-visible text or granting broad
filesystem access.

This spec defines the virtual file semantics for `merry_artifacts://` URIs.
It is intentionally limited to read-only, session-level artifact blobs.

## Decision

Expose selected runtime artifacts as read-only virtual files addressed by
`merry_artifacts://` URIs.

The URI is a capability-bearing reference interpreted by the Merry runtime and
tool execution layer. It is not a host filesystem path, not a network URL, and
not a persistent cross-session identifier.

Primary shape:

```text
merry_artifacts://session/<artifact-id>/<logical-name>
```

Example:

```text
merry_artifacts://session/art_01JZ7G4KJ8Q2/stdout.txt
```

The `session` authority means "the current runtime session that issued this
tool call". A URI produced in one session must not resolve in another session
unless a future explicit export/import feature creates a new artifact in the
target session.

## Goals

- Give shell tools a file-like input surface for exact artifact bytes.
- Avoid copying large tool outputs into prompts or temporary workspace files.
- Preserve artifact read access under Merry's existing permission and evidence
  model.
- Make artifact reads observable and auditable without treating artifacts as
  mutable workspace state.
- Keep the first design small enough to implement without a general virtual
  filesystem.

## Non-Goals

- No write support through `merry_artifacts://`.
- No directory mutation, glob expansion, symlinks, hard links, or file locking.
- No cross-session artifact addressing.
- No stable public URL or external sharing contract.
- No replacement for checkpoint reference lookup tools.
- No requirement that every tool understands these URIs directly.
- No host path disclosure for persisted artifact storage internals.

## URI Syntax

The only accepted authority for the first version is `session`:

```text
merry_artifacts://session/<artifact-id>/<logical-name>
```

Rules:

- The scheme is exactly `merry_artifacts`.
- The authority is exactly `session`.
- `<artifact-id>` is a validated Merry artifact ID, not an arbitrary path
  segment.
- `<logical-name>` is display and file-type metadata chosen by the runtime.
- URI path segments are percent-decoded only after structural parsing.
- `.` and `..` path segments are rejected.
- Empty artifact IDs, empty logical names, fragments, usernames, passwords, and
  ports are rejected.
- Query parameters are reserved and rejected in the first version.

The logical name does not participate in identity. It exists so process tools
and diagnostics can present useful names such as `stdout.txt`, `stderr.txt`,
`result.json`, `patch.diff`, or `snapshot.txt`. If the logical name in a URI no
longer matches the registry metadata for the artifact, the runtime should reject
the URI rather than silently serving a surprising blob.

## Artifact Identity

An artifact URI identifies one immutable blob in the current session artifact
registry.

The runtime must resolve the artifact ID through session state, not by parsing
host storage paths. Resolution succeeds only when:

- the artifact exists in the current session;
- the artifact has exact content available;
- the artifact is marked readable by the tool execution scope; and
- the URI logical name matches the artifact's registered logical name or an
  explicitly registered alias.

Artifacts remain immutable after recording. If a later operation needs derived
content, it must create a new artifact with a new ID and new URI.

## Read Semantics

Reads are byte-for-byte reads of the stored artifact content.

The runtime may expose metadata alongside the virtual file, including:

- byte length;
- media type or artifact kind;
- logical name;
- source action or tool call ID;
- created sequence number; and
- content digest when available.

Read APIs must not normalize line endings, transcode text, strip ANSI escapes,
or apply prompt-output truncation. If a consumer requests text decoding, decode
errors belong to that consumer or to an explicit text-read adapter, not to the
artifact URI resolution layer.

Partial reads are allowed. A bounded read must return the requested byte range
or a clear range error. Range reads do not create new artifacts unless a tool
explicitly records its own output as a separate result artifact.

## Shell Tool Integration

Process tools may support artifact URIs in two ways:

1. Pre-open or mount selected artifact blobs as read-only virtual files before
   process execution.
2. Rewrite declared URI arguments to sandbox-local read-only file paths that
   point at runtime-managed materializations.

Both approaches must preserve the same product contract:

- the child process can read only artifacts explicitly admitted to the action;
- the child process cannot infer the host artifact store path;
- materialized files are read-only from the child process perspective;
- materialized paths are scoped to one process action and may disappear after
  the action completes; and
- stdout/stderr and process metadata are still recorded as ordinary result
  artifacts.

The runtime may choose either implementation per platform. User-facing behavior
must not depend on whether the artifact was mounted, copied, linked, or streamed
internally.

## Permission Model

`merry_artifacts://` reads are governed by Merry tool policy, not by host
filesystem policy alone.

A process action that references an artifact URI must declare that read before
execution. The permission review should show the artifact ID, logical name,
kind, byte length, and source summary when available. It should not show full
artifact contents by default.

Artifact read access is narrower than workspace read access:

- granting workspace read access does not grant artifact URI access;
- granting artifact URI access does not grant workspace read access;
- granting one artifact URI does not grant all artifacts in the session; and
- artifact access never grants network access.

If an artifact contains sensitive data, normal policy review and redaction rules
still apply to summaries and diagnostics. The exact blob remains available only
through an authorized artifact read path.

## Prompt Projection

Artifact URIs may appear in model-visible tool results, checkpoint references,
or diagnostics as compact handles, but the URI itself must not imply that the
model can read the content without a tool call.

The context compiler should treat a URI as a reference, not as content. Full
artifact bytes are projected only when an explicit context policy or read tool
selects them.

This keeps the existing separation intact:

- artifacts own exact evidence;
- ledger facts own structured observations; and
- prompt context owns selected projections.

## Lifetime And Resume

Artifact URIs are stable for the lifetime of a resumable session as long as the
referenced artifact remains in the session artifact registry.

After session resume, the same URI should resolve to the same bytes if:

- the session ID is the same;
- the artifact registry was restored successfully;
- the artifact content is available; and
- the artifact ID and logical name still match registry metadata.

The URI does not promise stability outside the session store. Exporting,
copying, or attaching an artifact to another session must mint a new artifact ID
in the receiving session.

## Error Handling

Resolution failures should be explicit and typed enough for tools and users to
understand what happened:

- `invalid_artifact_uri`: syntax, authority, query, fragment, or path segment is
  invalid.
- `artifact_not_found`: the artifact ID is not present in the current session.
- `artifact_name_mismatch`: the logical name does not match registry metadata.
- `artifact_content_unavailable`: metadata exists but exact bytes cannot be
  loaded.
- `artifact_read_denied`: policy did not grant this artifact to the action.
- `artifact_range_invalid`: a requested byte range is outside the artifact.

Errors should include the artifact ID when it was syntactically valid and safe
to report. Errors must not disclose host storage paths.

## Observability

Artifact URI reads should produce bounded runtime diagnostics or events that
record:

- artifact ID;
- logical name;
- byte length or range;
- consuming tool/action ID;
- success or failure code; and
- whether materialization was used, without exposing the materialized host path
  unless it is already sandbox-local and safe to display.

Diagnostics should avoid logging full artifact contents by default.

## Acceptance

- A process action can read an explicitly granted `merry_artifacts://session/...`
  URI as a file-like input without broad workspace access.
- A process action that references an ungranted artifact URI is denied before
  execution or fails with `artifact_read_denied` without reading bytes.
- The runtime never exposes artifact store host paths in the public URI or
  default diagnostics.
- Artifact URI reads preserve exact bytes and do not apply prompt truncation.
- The same session resumed from durable state can resolve the same artifact URI
  to the same bytes.
- Invalid authorities, path traversal segments, fragments, queries, and logical
  name mismatches are rejected deterministically.
- Result artifacts created by consuming tools remain ordinary session artifacts
  with new IDs.
