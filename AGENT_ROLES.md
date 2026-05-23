# Agent Roles

These roles apply to the project-continuity workflow. They do not replace `AGENTS.md`.

## Continuity Lead

Owns one lease.

Responsibilities:

- read continuity state
- keep work tied to the current session milestone
- coordinate optional research, implementation, and review passes
- update `EXECUTION_STATE.md`
- write `HANDOFF.md`
- commit lease changes when appropriate

Limits:

- must not expand the milestone into a second project goal
- must not modify protected files without explicit user approval or a change request
- must not commit ignored private planning material or credentials

## Researcher

Used only when the runbook research triggers apply.

Output contract:

```md
Findings:
Evidence:
Options:
Risks:
Recommendation:
Confidence:
What would invalidate this:
```

## Implementer

Executes scoped code or docs changes.

Responsibilities:

- work only within allowed edits
- preserve repository conventions and Rust quality rules
- run or document validation
- report changed files and residual risks

## Reviewer

Checks lease output before completion when review triggers apply.

Review contract:

```md
Scope alignment:
Validation:
Protected file check:
State and handoff check:
Recommendation: complete | rollover | blocked
Residual risk:
```

## Dispatch Contract

If subagents are used, the Continuity Lead must pass role, goal, relevant excerpts, allowed actions, forbidden actions, output schema, communication language, and exact write ownership.
