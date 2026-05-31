# Skill Progressive Disclosure

Date: 2026-05-31

## Purpose

Merry needs a practical skill mechanism for reusable prompt instructions without
inventing a new ecosystem. The first useful version should consume existing
filesystem-based `SKILL.md` directories, expose only lightweight discovery
metadata up front, and let the model read full skill instructions only when the
task needs them.

This is a runtime context feature, not a plugin framework. It should make Merry
usable as a coding/runtime SDK while keeping context growth bounded and avoiding
a second, Merry-specific skill format.

## External References

This design follows two observed patterns:

- Anthropic Agent Skills document a three-level loading model: YAML
  frontmatter metadata is always loaded, `SKILL.md` instructions load when the
  skill is relevant, and bundled resources or scripts load only as needed.
  Source: <https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview>
- Codex's local implementation scans skill roots for `SKILL.md`, renders
  available skill metadata into model-visible instructions, and relies on
  filesystem reads for full skill bodies.
  Local evidence: `.merry/codex/codex-rs/core-skills/src/loader.rs` and
  `.merry/codex/codex-rs/core-skills/src/render.rs`.

## Current Position

These points are the current Merry design constraints:

- Skill compatibility means filesystem compatibility first.
- A skill is a directory containing a required `SKILL.md`.
- The required `SKILL.md` frontmatter fields for the MVP are `name` and
  `description`.
- Do not add a separate `trigger` field in the MVP. Existing skill ecosystems
  usually encode "when to use this" in `description` and body text.
- Runtime must not become a semantic skill selector. The model chooses whether
  a visible skill is relevant.
- Merry runtime surfaces that expose skills must also expose normal file-read
  capability. In the current runtime this is `workspace_read_file`.
- There is no no-file-read skill mode and no `skill_read(skill_id)` fallback in
  this spec.
- Full `SKILL.md` body text is dynamic context. It is not part of the stable
  prefix.
- Skill references, scripts, assets, and templates are not loaded until the
  skill body asks for them and the model chooses the needed file or command.
- Tool permission, sandbox, cwd, and path policy still apply. Skills do not get
  special authority.

## Non-Goals

- Do not create a Merry-specific skill authoring format.
- Do not create a plugin marketplace or plugin bundle system in this slice.
- Do not implement subagents in this slice.
- Do not implement a deterministic runtime classifier for skill activation.
- Do not implement automatic pinning, long-term memory, or an activation
  evidence graph for skills.
- Do not inject every skill body into context at startup.
- Do not auto-execute skill scripts. Scripts run only through existing tool
  execution paths and policy.
- Do not use provider-specific hosted skill APIs as runtime state.
- Do not let Python bindings reimplement skill loading or context assembly.

## Skill Directory Shape

MVP-compatible skill:

```text
some-skill/
  SKILL.md
  references/
  scripts/
  assets/
```

Only `SKILL.md` is required.

Minimum `SKILL.md`:

```markdown
---
name: some-skill
description: What this skill does and when to use it.
---

# Some Skill

Instructions for the model.
```

The MVP parser should accept the standard frontmatter fields:

```text
name: string
description: string
```

Additional frontmatter should be ignored unless a later spec promotes it into
the Merry contract.

## Skill Roots

The host config should provide one or more skill roots.

Examples:

```toml
[skills]
enabled = true
roots = [
  "~/.codex/skills",
  "~/.claude/skills",
  "./skills",
]
```

Python SDK shape should stay equivalent:

```python
runtime = merry.Runtime(skill_dirs=[
    "~/.codex/skills",
    "~/.claude/skills",
    "./skills",
])
```

The Rust runtime owns loading and context compilation. Python only passes
directories and receives runtime events/errors.

## Discovery

Runtime scans configured roots for `SKILL.md` files and builds a skill catalog:

```text
SkillMetadata:
  name
  description
  skill_md_path
  root
```

The first implementation slice does not expose a public `SkillId`; it uses
normalized skill names internally for deterministic ordering and duplicate
handling. A runtime-stable `SkillId` derived from normalized name and path can
be added later when another API needs to reference a skill directly.
`name` remains the model-visible skill name from frontmatter.

The catalog is a runtime input to context compilation. It is not a durable
memory store.

Duplicate handling should be deterministic:

- Prefer the first root in configured order.
- If two skills have the same normalized name under the same root priority,
  keep one deterministic winner and report the skipped duplicate in diagnostics.
- Exact policy can evolve, but it must be deterministic and visible in tests.

Malformed skills should not crash the whole runtime unless strict mode is
enabled later. The MVP should skip invalid skills and report structured load
warnings.

## Stable Prefix Projection

Skill metadata is projected as an available-skills section in the stable prefix.
It should appear after the base runtime instructions and before project rules
such as `AGENTS.md`.

The intended provider-visible order is:

```text
stable prefix:
  runtime base instructions
  available skill metadata
  project rules, if any
  provider tool schemas / function declarations

dynamic context:
  task anchor
  compacted checkpoint
  compiled context / memory projection
  append-only user and assistant body
  current user input
```

Skill metadata belongs here because it usually changes only when the configured
skill roots or `SKILL.md` frontmatter change. Keeping it before task-specific
context lets provider prompt caching reuse it across turns.

Projected metadata contains only:

```text
name
description
path to SKILL.md
```

It does not contain:

```text
full SKILL.md body
references
scripts
assets
derived trigger rules
activation history
```

The model-visible instruction should say:

- The list is for discovery.
- If the user explicitly names a skill, use it for that turn.
- If the task clearly matches a skill description, read that skill's
  `SKILL.md` before relying on it.
- Use `workspace_read_file` to read the listed `SKILL.md`.
- Resolve relative paths mentioned by `SKILL.md` relative to that skill
  directory.
- Read only the referenced files needed for the task.
- Do not carry a skill body across unrelated turns unless it remains in raw
  context or is re-read.

The available-skills section should affect the stable-prefix hash. A changed
skill root list, added or removed skill, or changed visible frontmatter should
invalidate that cache prefix. Changing only the hidden `SKILL.md` body does not
need to invalidate the prefix unless the body change also changes visible
metadata.

## Loading Full Skill Bodies

Full skill body loading is model-directed:

```text
available skill metadata visible
  -> model decides skill is relevant
  -> model calls workspace_read_file(path_to_skill_md)
  -> file content enters normal tool-result context
  -> model follows the skill instructions for the current task
```

This is intentionally not "clean" after the body is read. The body becomes
context like any other file read or tool result. The benefit is that irrelevant
skill bodies never enter the context window.

When a skill body points to additional files:

```text
references/api.md
scripts/validate.py
assets/template.docx
```

the model should access only the specific file or script needed. Merry should
not pre-load a skill directory.

## File-Read Requirement

There is no skill-specific read tool in this design.

Any runtime surface that enables skills must provide a general file read tool
capable of reading configured skill files. For the current coding/runtime path,
that tool is `workspace_read_file`.

Any runtime surface without general file-read capability cannot enable
filesystem skills. Adding `skill_read` as a parallel mechanism would duplicate
capability, complicate prompt rules, and make SDK behavior diverge from the
normal workspace tool model.

## Path And Permission Policy

Skills are not a permission bypass.

The runtime should allow `workspace_read_file` to read configured skill roots
when skills are enabled. The same path policy should decide whether referenced
files, scripts, and assets are readable or executable.

Minimum policy:

- `SKILL.md` files under configured skill roots are readable.
- Relative paths from a skill body resolve from the skill directory.
- Reads outside configured skill roots follow normal workspace policy.
- Script execution follows normal process/sandbox policy.
- Network access is not implied by installing or using a skill.

## Compaction Interaction

Skill metadata can be regenerated from the skill catalog and should not be
captured as checkpoint substance.

Full skill bodies and skill reference reads are normal context once read. They
may be covered by checkpoint compaction like other tool-result content, but the
checkpoint should summarize only task-relevant consequences, not restate the
whole skill.

Good checkpoint claim:

```text
The frontend-design skill was used for this turn, and its constraints require
checking responsive text fit before completion.
```

Bad checkpoint claim:

```text
Full copied body of frontend-design/SKILL.md...
```

If the later model needs exact skill instructions again, it should re-read the
current `SKILL.md`.

## SDK Contract

The Python SDK should expose skill roots as configuration only:

```python
runtime = merry.Runtime(
    config=merry.RuntimeConfig(
        skill_dirs=["./skills", "~/.codex/skills"],
    )
)
```

It should not expose a Python callback for "select skill" in the MVP. The
runtime compiles metadata; the model decides; the existing file-read tool reads
the selected body.

Errors should use the future `MerryErrorInfo` public shape, for example:

```text
skill.load_failed
skill.invalid_frontmatter
skill.duplicate_skipped
skill.root_unreadable
```

Warnings for skipped skills should be visible in diagnostics/events, but should
not fail the run by default.

## First Implementation Slice

The first vertical slice should prove:

1. Config can register one or more skill roots.
2. Runtime scans roots and parses `name`/`description` from `SKILL.md`.
3. Context projection includes skill metadata but not full bodies.
4. The projected instructions tell the model to use `workspace_read_file` for
   the full body.
5. Tests verify body text is absent from the initial projection.
6. Tests verify malformed skill files are reported and skipped.
7. A smoke task can read a listed `SKILL.md` through `workspace_read_file`.

Suggested deterministic tests:

```text
loads_skill_metadata_from_skill_roots
projects_skill_metadata_without_body
skips_invalid_skill_frontmatter_with_warning
skill_body_can_be_read_with_workspace_read_file
```

No live model test is required for the first implementation slice. Model
selection quality can be evaluated later with an ignored smoke test after the
mechanics are deterministic.

## Open Questions

These should stay out of the MVP unless they become blockers:

- Whether skill metadata should reload every turn, every session, or only when
  explicitly refreshed.
- Whether duplicate-name policy needs scopes such as project/user/system.
- Whether plugins should later bundle skills, MCP servers, apps, and config.
- Whether explicit user syntax such as `$skill-name` should produce a direct
  body injection event instead of relying on the model to read the file.
- Whether skill usage should emit a dedicated runtime event after the model
  reads a skill body.
