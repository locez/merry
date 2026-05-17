# Subagent Workflow

This file defines how Merry uses subagents during development. It is about project execution, not the future runtime subagent feature.

## Principles

- Subagents are bounded workers.
- Work is assigned by contract, not by vague intent.
- Parallel work requires disjoint ownership.
- Research agents do not edit files.
- Implementation agents own specific files or modules.
- Review agents inspect concrete diffs, tests, and requirements.

## Task Types

### Research

Use for uncertain engineering choices that affect architecture.

Prompt must include:

- exact question
- local files to read
- whether web research is required
- source quality expectation
- output format

Expected output:

```text
adopt now
defer
risks
rules to add
source links
```

Research agents must not edit files.

### Implementation

Use for scoped code changes with clear ownership.

Prompt must include:

- owned files/modules
- files that are read-only context
- behavior to implement
- tests to write or run
- constraints from `AGENTS.md`
- expected final report

Implementation agents must not edit outside their scope unless the lead expands it.

### Spec Review

Use to verify implementation matches the written task or plan.

The reviewer should answer:

- What requirement is satisfied?
- What requirement is missing?
- What extra behavior was added?
- Which files/lines support the finding?

### Code Quality Review

Use to verify idiomatic Rust and repository standards.

The reviewer should focus on:

- unnecessary clone/allocation
- ownership/API shape
- unsafe usage
- async correctness
- error quality
- public API stability
- provider/runtime boundary leaks
- test quality

## Standard Implementation Prompt Shape

```text
You are implementing one scoped task in the Merry repository.

You are not alone in the codebase. Do not revert or overwrite edits made by others.

Owned scope:
- ...

Read-only context:
- ...

Goal:
- ...

Constraints:
- Follow AGENTS.md.
- Keep changes scoped.
- Do not commit private docs.
- Do not use unsafe.
- Do not clone to satisfy the borrow checker.

Verification:
- ...

Final report:
- status: DONE / DONE_WITH_CONCERNS / NEEDS_CONTEXT / BLOCKED
- files changed
- tests/checks run
- notable design choices
- concerns or follow-up
```

## Standard Research Prompt Shape

```text
You are researching one architecture decision for Merry.

Do not edit files.

Question:
- ...

Local context:
- Read AGENTS.md.
- Read specific docs/files if needed.

Research expectations:
- Prefer primary or official sources.
- Use recent sources for fast-moving libraries/providers.

Return:
- adopt now
- defer
- risks/tradeoffs
- concrete repository rules or implementation tasks
- source links
```

## Integration Rules

The lead agent integrates all subagent output.

Before accepting a subagent result:

- inspect changed files or memo claims
- check scope compliance
- check conflicts with other work
- run relevant verification locally when feasible
- update plans or guardrails only after reconciling disagreements

Subagent output is evidence, not authority.

