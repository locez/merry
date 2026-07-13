# Recursive Plan Tree And Plan Mode

Date: 2026-07-13

## Purpose

Merry currently has a flat, runtime-owned subagent batch model and a TUI built
around one chronological timeline. It does not have a durable plan model, a
planning execution policy, plan approval, recursive task projection, or a plan
view.

This spec defines the complete usable implementation:

```text
The main coordinator, a governing skill, or the user can activate Plan Mode;
the model can create a recursive durable task tree without changing the
session's provider-visible tool contract, the user can inspect and revise that
tree in the TUI, and the runtime can execute ready leaves concurrently through
bounded workers while results, failures, revisions, and recovery remain durable
and visible.
```

The implementation must prove the complete value of recursive planning: lazy
decomposition, selective context, safe true concurrency, result folding,
typed recovery, permission-independent plan control, responsive UI, and resume
after interruption.

## User-Approved Product Decisions

The following decisions were confirmed during the design discussion:

- A plan is a durable recursive task tree, not a flat checklist and not a
  transcript decoration.
- A task node is separate from an agent run. An agent run is an execution
  attempt or lease against a node.
- Plan trees expand lazily along the active frontier. They are not decomposed
  to maximum depth at startup.
- Plan Tree, Plan View, and Plan Mode are separate concepts.
- The main coordinator may activate Plan Mode on its own judgment, including
  when a governing skill requires planning. User activation remains an optional
  control surface, not a prerequisite.
- Plan Mode is structured coordination state, not a read-only sandbox. It does
  not add, remove, reorder, or rewrite provider-visible tools and does not
  change the current permission envelope.
- Coordinator tool names, descriptions, schemas, and order remain stable for
  the lifetime of the session so plan phase changes do not invalidate reusable
  model-request prefixes.
- During execution, the main agent may revise future, unstarted subtrees when
  implementation evidence invalidates the earlier plan.
- A valid plan may enter execution directly when its complete capability
  envelope is covered by existing user authorization and neither the user nor a
  governing skill requested plan review.
- `AwaitingApproval` is reserved for permission/capability expansion, a change
  to the established root objective or acceptance contract, explicit review
  requests, destructive external authority, or required external input.
- Users are not required to estimate wall-clock, token, or attempt budgets.
  Runtime/operator resource policy and coordinator progress review govern long
  execution; user approval remains focused on objective and permission changes.
- Node context is compiled from the root contract, ancestor path, current node,
  explicit dependency results, selected evidence, and the node-local working
  tail. Unrelated sibling transcripts are not inherited.
- The long-term scheduler is hybrid: the model declares node execution intent
  and harness constraints; the runtime decides when a ready leaf can safely
  receive an execution lease.
- A worker may directly decompose its leased subtree inside the authorized
  envelope. Routine local expansion does not require main-coordinator review.
- The main coordinator may send persisted attempt-scoped steering only for
  anomalies, user intervention, requested review, or global coordination; it
  does not supervise routine worker reasoning.
- One runtime-owned `PlanController` serializes every plan mutation and durable
  commit from coordinator, workers, user controls, and scheduler logic.
- Plan authoring uses a stable hybrid update contract: full-tree `DefinePlan`
  while planning and node-revision-guarded `ReplaceSubtree` for mutable future
  work during execution.
- One attempt spans multiple model turns and tool calls under one lease. Yield,
  terminal outcome, lease loss, interruption, or decomposition ends it; retries
  and child work receive new attempts.
- Failure recovery is typed: transient failures may retry, semantic failures
  return to parent replanning, and boundary changes block for the user.
- The TUI uses a hybrid layout: timeline plus persistent plan tree on suitable
  widths, and a full-screen plan overlay on narrow terminals.

## Delivery Capability

The implementation must deliver one real interactive workflow:

```text
waiting session
-> coordinator, governing skill, or user activates Plan Mode
-> model inspects with the session's existing tools and permission policy
-> model creates or revises a recursive plan
-> runtime durably records the plan
-> TUI renders timeline plus plan tree
-> runtime executes directly when existing authorization covers the plan,
   otherwise it enters awaiting approval with explicit reasons
-> runtime derives dependency-ready leaves
-> bounded workers receive scoped execution leases and run concurrently
-> a worker may finish its leaf or lazily expand descendants under that leaf
-> compact results and exact evidence fold into parent nodes
-> typed failures retry, trigger replanning, or block at approval boundaries
-> newly ready leaves continue until root acceptance completes or blocks
-> session save/resume reconstructs the plan, attempts, leases, and scheduler
```

The scheduler reuses or refactors the existing child-runtime/subagent execution
infrastructure. Workers remain bounded executors and do not own the global task
tree.

## Acceptance Scenario

The primary deterministic acceptance test uses a fake provider and no network:

1. Start an interactive runtime with workspace read/write tools, a child
   runtime factory, and plan control tools registered.
2. The coordinator calls `begin_plan` while the runtime is active or waiting.
3. The provider-visible coordinator tool definitions and ordering remain
   identical before and after plan activation.
4. The fake provider reads evidence and creates a plan whose active branch has
   grandchildren.
5. The runtime records plan revision 1 and exposes a public plan event.
6. Plan activation leaves the existing workspace-write permission decision
   unchanged; plan state neither grants nor revokes the action.
7. The coordinator requests execution, and the runtime verifies that every node
   harness stays inside the existing user authorization envelope.
8. The runtime enters `executing` without a second approval because no boundary
   changed.
9. The TUI renders the recursive tree alongside the timeline at 140x40 and as
   an overlay at 50x20.
10. The runtime derives two dependency-ready leaves with disjoint write scopes
   and starts both child runtimes concurrently.
11. One child completes with evidence. The other child lazily expands its
    leased node into two grandchildren and ends its current attempt as
    `decomposed` without coordinator review.
12. The scheduler starts the new ready grandchildren without giving the child
    direct subagent-spawn authority.
13. One transient attempt fails before observable side effects and is retried
    because its recovery policy still permits another attempt.
14. A semantic failure is folded into the plan and wakes the coordinator,
    which revises an unstarted future subtree without changing the established
    root contract.
15. A long-running live child triggers progress review; the coordinator sends a
    persisted `converge` directive, runtime waits for the next safe provider
    boundary, and the child acknowledges and applies it without exposing its
    transcript.
16. Two worker terminal reports arriving concurrently are serialized by
    `PlanController` without lost updates or scheduling before persistence.
17. A dependent validation node starts only after prerequisite results are
    durably committed.
18. Save and reload preserve the plan id, revision, phase, node ids, statuses,
    dependencies, results, attempts, checkpoints, directive history,
    expired/interrupted leases, and superseded nodes without duplicating
    completed work or replaying attempt-scoped directives into successors.

## Scope Guard

### Included

- Runtime-owned plan, node, attempt, lease, and directive ids; phases, node and
  directive statuses, executor hints, dependency edges, harness contracts,
  recovery policies, progress, results, and revisions.
- A single-writer `PlanController` with bounded typed command channels and
  resume-safe transaction ordering.
- Stable coordinator/worker plan tools with optimistic revision and lease
  checking, full-tree `DefinePlan`, execution-time `ReplaceSubtree`, progress,
  terminal reporting, and attempt-scoped steering.
- Durable session persistence and public runtime events for plan state.
- Plan-aware context projection for the coordinator and workers.
- Autonomous or user-triggered plan activation, stable coordinator tool
  definitions, permission-independent plan state, direct execution inside
  existing authorization, and explicit TUI approval only at real boundaries.
- Future-subtree revision with immutable completed/running node protection.
- A central bounded scheduler that derives ready leaves, rejects capability or
  write-scope conflicts, and assigns execution leases.
- Worker-scoped lazy decomposition: a worker may atomically report direct
  children below its leased node and release the current attempt.
- Concurrent child execution, compact result folding, dependency release, and
  parent verification.
- Typed bounded retry, coordinator replanning, boundary blocking, lease expiry,
  and crash/restart recovery.
- Timeline plan-revision/steering entries, a responsive plan tree, and a
  read-only node inspector with worker, attempt, progress, directive, and
  approval state.
- Deterministic runtime, scheduler, persistence, context, event, and TUI tests.

### Deferred Until Supported By Usage Evidence

- Direct child-to-child spawning. Task recursion is intentionally owned by the
  central plan scheduler rather than by an agent process hierarchy.
- Multi-host or distributed workers, work stealing, and unbounded concurrency.
- Runtime invention of semantic tasks or harnesses without a model-authored
  plan contract.
- Cross-plan dependencies, arbitrary external workflow graphs, and intelligent
  merge/conflict resolution.
- Deep Python callbacks from worker runtime internals.
- A web or graphical plan UI.

These are separate capabilities, not missing parts of the approved recursive
plan design. This design executes arbitrary-depth task leaves through central
scheduling and therefore validates recursive decomposition and true concurrency
end to end.

### Implementation Naming

Plan Tree and Plan Mode implementation names describe stable domain concepts,
not delivery stages. New modules, types, traits, functions, events, commands,
configuration keys, persistence fields, and tests must not use `V1`, `v1`,
`_v1`, or equivalent first-version suffixes or prefixes.

Use semantic names such as `PlanState`, `PlanAttempt`, `PlanLease`, and
`CoordinatorDirective`. If a future incompatible serialized representation
requires migration, version that persistence schema independently at its
serialization boundary; do not rename the domain model or public runtime API.

## Ownership Boundaries

- `merry-core` owns serialized plan/directive ids, snapshots, public status and
  approval enums, and runtime event payloads.
- `merry-runtime` owns `PlanController`, plan validation, revisions, session
  persistence, context projection, intrinsic plan controls, ready-leaf
  derivation, execution leases, attempt recovery, steering delivery, result
  folding, and interactive phase transitions.
- `merry-cli` owns command-palette actions, local plan selection/folding state,
  and Ratatui rendering.
- Provider crates receive ordinary Merry tool schemas and normalized events;
  they do not receive provider-specific plan types.
- Existing subagent/child-runtime infrastructure is refactored behind the plan
  scheduler as bounded worker execution. It does not become the owner of the
  plan tree.

## Authority And Autonomy

| Actor | Owns | Must Not Do |
| --- | --- | --- |
| User | Root intent, acceptance changes, permission/capability expansion, destructive external authority, explicit review requirements | Estimate runtime duration or supervise routine worker decisions |
| Main coordinator | Initial plan, execution intent, mutable future-subtree revision, global synthesis, anomaly steering, final acceptance judgment | Approve new user permissions, rewrite live/completed work, or review every routine child expansion |
| Worker/local attempt | Execute one leased node, choose tools inside its harness, report progress/result, directly decompose its leased subtree inside the authorized envelope | Rewrite ancestors/siblings, create cross-plan work, expand permissions, or spawn an opaque child-agent hierarchy |
| Scheduler | Derive ready work, conflicts, capacity, retry/requeue candidates, and deterministic follow-up commands | Invent semantic tasks or mutate plan state outside `PlanController` |
| Runtime/PlanController | Validate hard invariants and actor authority, serialize/persist mutations, enforce scopes/permissions/directives, publish committed events | Decide whether a semantic investigation is valuable or cancel healthy work solely because time elapsed |

The default rule is local autonomy inside an explicit envelope. Coordinator or
user review occurs only when a worker requests it, adaptive progress review
finds an anomaly, global synthesis/replanning is required, or a root/permission
boundary would change.

## Core Data Model

### Identifiers

Add validated newtypes:

```rust
PlanId
PlanNodeId
PlanAttemptId
PlanLeaseId
PlanDirectiveId
PlanApprovalRequirementId
```

The runtime assigns all authoritative ids. Provider input may reference ids
returned by prior plan tool results but cannot choose authoritative ids for new
nodes, attempts, leases, directives, or approval requirements.

### Plan Phase

One session may have zero or one active plan:

```rust
enum PlanPhase {
    Planning,
    AwaitingApproval,
    Executing,
    Completed,
    Blocked,
    Cancelled,
}
```

Sessions without a plan keep current behavior.

Phase transitions are runtime-owned:

```text
no plan -> planning                 (coordinator tool or user control)
planning -> executing               (execute_if_authorized and no review reason)
planning -> awaiting_approval       (authorization or explicit review required)
awaiting_approval -> planning     (user requests revision)
awaiting_approval -> executing    (requirements resolved and user approves)
executing -> planning             (user reopens an approval boundary)
executing -> awaiting_approval    (established root/permission boundary changes)
executing -> completed|blocked|cancelled
```

`begin_plan` is a provider-visible coordinator control and an optional
interactive user command backed by the same runtime transition. The runtime
commits activation at the tool or interactive command boundary; activation does
not change the registered coordinator tools or the existing action-admission
policy.

### Node Status

Store semantic status, not every derived scheduling label:

```rust
enum PlanNodeStatus {
    Pending,
    InProgress,
    Expanded,
    Verifying,
    Completed,
    Blocked,
    Failed,
    Superseded,
}
```

`ready` is derived when a pending leaf has no incomplete dependencies, its
ancestors are executable, its capability scopes do not conflict with live
leases, and scheduler capacity exists. `running` is a UI label for
`InProgress`. `Expanded` means the node was lazily decomposed and now waits for
descendant completion before its own verification.

### Executor And Harness Contract

Each executable node carries the model-authored execution intent:

```rust
enum PlanExecutorPolicy {
    Local,
    Delegate,
    Auto,
}
```

Semantics:

- `Local` reserves the node for the main coordinator runtime.
- `Delegate` requires a bounded child worker.
- `Auto` lets the scheduler choose local execution or a child worker under
  capacity and policy constraints.

Each node also carries a bounded harness contract:

```text
PlanHarnessSpec:
  model_role?
  reasoning_effort?
  checkpoint_turn_interval?
  provider_request_timeout?
  tool_timeout?
  allowed_tools[]
  read_scope[]
  write_scope[]
  forbidden_paths[]
```

The model authors or revises this contract inside the user-authorized capability
ceiling. The runtime may narrow it but never expand it. Provider/model role
resolution uses existing Merry role/provider configuration.

### Recovery Policy

```text
PlanRecoveryPolicy:
  max_transient_attempts
  retry_backoff
  retry_only_before_observable_side_effects
```

The policy is bounded and validated. Semantic failures do not consume a blind
retry loop; they return evidence to the coordinator for replanning.

### Runtime Resource And Progress Policy

```text
PlanResourcePolicySnapshot:
  max_concurrency
  worker_heartbeat_interval
  worker_heartbeat_ttl
  provider_request_timeout
  tool_timeout
  checkpoint_turn_interval
  no_durable_progress_review_window
  repeated_failure_limit
```

This policy comes from runtime/operator configuration and provider defaults,
not from required user estimation. It is persisted as a snapshot so resume and
diagnostics remain reproducible. The existing subagent `max_threads` value is
the hard concurrency ceiling.

There is no default wall-clock, token, or total-attempt ceiling for a plan or
node. Elapsed time and reported usage are diagnostics. A task continues while
it makes useful progress or the coordinator can find a reasonable next action.
Optional user deadlines or platform/account quotas apply only when explicitly
configured and produce typed external constraints rather than inferred task
failure.

Scheduler admission state is explicit:

```rust
enum PlanSchedulerStatus {
    Active,
    Paused,
    Draining,
}
```

`Draining` stops new leases and waits for live attempts to settle during plan
cancellation or approval-boundary transitions.

### Plan Node

The persisted internal shape is flat for stable ids, dependency validation,
and projection, while the public/UI snapshot exposes parent/order fields that
can be rendered recursively:

```text
PlanNode:
  id
  parent_id?
  sibling_order
  objective
  acceptance[]
  status
  executor_policy
  harness
  recovery_policy
  depends_on[]
  result?
  created_revision
  updated_revision
```

Text and collection sizes must be bounded. Initial limits:

- at most 128 non-superseded nodes;
- at most 16 direct children per node;
- at most 16 dependency ids per node;
- objective at most 2 KiB UTF-8;
- each acceptance item at most 1 KiB UTF-8;
- at most 16 acceptance items;
- maximum task depth 16;
- at most 8 transient retry attempts per node revision;
- at most 32 non-terminal directives per attempt;
- directive reason/instruction and progress summary at most 2 KiB UTF-8 each;
- at most 16 requested-output items or evidence/artifact refs per progress or
  directive payload, each text item at most 1 KiB UTF-8;
- at most 256 KiB for one serialized plan snapshot.

These are safety limits, not a claim that healthy plans should approach them.

### Node Result

```text
PlanNodeResult:
  conclusion
  evidence_refs[]
  artifact_refs[]
  changed_paths[]
  verification[]
  open_questions[]
```

Completing a node requires a non-empty conclusion. Coordinator-authored refs
must exist in the root session. Worker-authored refs are promoted into the root
session before the result commits. Exact payloads remain in artifact storage;
plan results remain compact navigation and acceptance records.

### Attempts And Leases

An agent session is not a task node. Every execution is recorded separately:

```text
PlanAttempt:
  attempt_id
  node_id
  node_revision
  lease_id
  executor_session_id
  harness_fingerprint
  started_at
  finished_at?
  outcome
  result?
  diagnostic?
  latest_checkpoint_ref?
  last_applied_directive_sequence

PlanLease:
  lease_id
  attempt_id
  node_id
  node_revision
  executor_session_id
  started_at
  last_heartbeat_at
  lease_expires_at
  status
```

Attempt outcomes are typed:

```text
completed
decomposed
blocked
semantic_failure
transient_failure
yielded
cancelled
interrupted
```

A `PlanAttempt` is one continuous execution episode for one node revision under
one lease. It normally maps to one worker runtime execution and may contain many
provider turns, tool calls, tool failures, model/provider transport retries,
artifacts, and `checkpoint_and_continue` operations. Those internal actions do
not create additional plan attempts.

An attempt becomes terminal only through a successful terminal report,
decomposition, cooperative yield, cancellation, lease loss, process/runtime
interruption, or a typed failure that returns the node to scheduling. A retry or
requeue always allocates a new attempt and lease. `checkpoint_and_yield` ends
the current attempt as `yielded`; its successor receives the durable checkpoint
through fresh scoped context.

A decomposed parent attempt ends before any child attempt starts. Every child
receives its own attempt, lease, and fresh context. The runtime may immediately
reuse the same physical worker slot for one ready child, but it must not reuse
the parent attempt id or inherit the parent transcript as the child's working
context.

At most one live lease may exist for a node revision. Cancellation tokens are
runtime-only and are reconstructed as cancelled/expired state after resume.
`lease_expires_at` is a renewable liveness deadline, not a task-duration
budget. The worker runtime renews it while alive, including while a provider
request or bounded tool call is in flight.
Public snapshots include bounded compact attempt views; exact diagnostics and
large outputs remain in artifacts and journal-backed state. Older terminal
attempts may be compacted out of the prompt/UI snapshot while remaining
recoverable through the journal and artifact references.

### Plan State

```text
PlanState:
  plan_id
  revision
  phase
  activation_source
  root_node_id?
  coordinator_node_id?
  execution_contract_fingerprint?
  execution_authorization_refs[]
  authorized_capability_envelope?
  approval_requirements[]
  nodes
  attempts
  leases
  attempt_progress
  directives
  resource_policy_snapshot
  max_concurrency_hint?
  scheduler_status
  revision_summaries
```

The root session owns `active_plan: Option<PlanState>` plus bounded references
to prior terminal plan snapshots/journal ranges. Historical plans do not become
children of the current `PlanState`.

```text
PlanActivationSource:
  coordinator { reason, governing_skill_id? }
  user

PlanApprovalRequirement:
  requirement_id
  kind
  status
  created_revision
  resolution_ref?

PlanApprovalRequirementKind:
  user_review_requested
  skill_review_requested { skill_id }
  root_objective_change
  root_acceptance_change
  capability_or_permission_expansion
  destructive_external_authority
  required_external_input { prompt }

PlanApprovalRequirementStatus:
  pending
  resolved
  rejected
```

`activation_source` records coordinator judgment with an optional validated
governing skill reference, or an interactive user command. Skills are not
runtime actors and never mutate plan state directly.
`execution_contract_fingerprint` covers the root objective, root acceptance
list, and user-authorized capability/permission envelope at the point execution
begins. It does not fingerprint the plan's
current incidental use of capabilities inside that envelope, so future subtree
revisions remain autonomous when they stay authorized. The fingerprint may be
established without a second approval when existing authorization covers the
plan. Later boundary changes populate typed `approval_requirements` and cannot
execute until resolved.

`execution_authorization_refs` cite runtime-owned user grants or task authority;
`authorized_capability_envelope` is a provider-invisible bounded snapshot of
tool and side-effect scopes and never contains credentials. They make admission
and resume diagnostics reproducible but do not override current permission
policy. A later user revocation immediately blocks new affected admissions and
creates a boundary requirement even when an older authorization ref exists.

An empty `Planning` state created by `begin_plan` may temporarily have no root
and no nodes. The first successful `DefinePlan` must install exactly one root.
An empty plan cannot enter `AwaitingApproval` or `Executing`.

## Plan Mutation Ownership

One runtime-owned `PlanController` is the sole writer for the active plan and
its persisted attempt/lease/control state. It runs as a serialized command
processor behind bounded Tokio channels rather than exposing a shared mutable
plan to coordinator tools, worker runtimes, interactive controls, and scheduler
tasks.

Typed command producers include:

```text
coordinator plan-control tool adapter
worker PlanWorkerControl handle
interactive user control handle
scheduler admission and recovery logic
heartbeat/progress recorder
```

Each command carries its runtime-derived actor identity and the expected plan,
node, attempt, lease, or directive revisions required by that operation. Model
input cannot claim a coordinator or worker identity.

The controller applies one mutation transaction at a time:

```text
receive command
-> validate actor, phase, revisions, lease, capabilities, and invariants
-> stage root-session artifacts, candidate state, and events
-> persist the complete resume-safe transaction
-> install committed state
-> reply to the caller
-> derive scheduler follow-up commands
-> expose public events
```

Worker terminal reports remain accepted while scheduling is paused or draining
so live attempts can settle. User pause/cancel commands take priority over new
lease-admission commands, but they do not discard already queued terminal
reports. The committed journal sequence is the authoritative ordering for
resume and replay diagnostics.

Generic `ToolExecutor` implementations remain outcome-only and do not call
runtime mutation APIs. Root coordinator plan tools are intrinsic runtime
controls that submit `PlanController` commands; worker plan tools use a scoped
handle to the root controller.

## Tree And Dependency Invariants

Every accepted non-empty plan update must prove:

- exactly one root exists;
- every live node is reachable from the root;
- parent links are acyclic;
- dependency links refer to live nodes in the same plan;
- dependency links are acyclic;
- a node cannot depend on itself or one of its descendants;
- sibling order is deterministic and unique within one parent;
- completed or in-progress nodes cannot be removed, reparented, or silently
  rewritten;
- omitted mutable nodes become `Superseded`; they are not deleted;
- a completed parent still requires its own result/acceptance transition;
  completed children do not automatically complete the parent;
- every live lease references the exact current node revision it owns;
- no node revision has more than one live lease;
- parallel live leases have non-overlapping write scopes and no conflicting
  exclusive capability;
- descendant tool and workspace capability envelopes never exceed their
  parent's delegable envelope or the authorized execution-contract ceiling;
- a decomposed attempt atomically adds at least one valid direct child below
  its leased node and transitions the node to `Expanded`;
- the main coordinator may focus one coordination path while multiple worker
  nodes execute concurrently.

## Provider-Visible Plan Tools

Register stable plan-control tools when constructing the coordinator and worker
runtimes. The coordinator tool set includes `begin_plan` from the first request;
plan activation never adds it dynamically.

```text
coordinator:
  begin_plan
  read_plan
  update_plan
  control_plan_attempt
  report_plan_progress
  report_plan_attempt

worker:
  report_plan_progress
  report_plan_attempt
```

`begin_plan` is coordinator-only. It records the activation reason, creates the
initial empty plan state when necessary, and returns the current phase and
revision. Repeated activation is idempotent only when it targets the same active
plan; it never grants permissions or changes general tool admission.

When the current plan is terminal, `begin_plan` preserves its final snapshot and
exact journal history, clears it from the active slot, and creates a new
runtime-owned `PlanId`. A session still has at most one active plan, while prior
terminal plans remain inspectable through persisted history/artifact refs.

`read_plan` returns an exact bounded active or terminal plan snapshot or selected
subtree by `PlanId`, including compact attempts, progress, and directive state
when requested. It is the coordinator's exact-read path when the normal context
projection omits unrelated branches or older attempt history.

```text
ReadPlanInput:
  plan_id?
  node_id?
  max_depth?
  include_attempts?
  include_progress?
  include_directives?
  cursor?
```

Omitted `plan_id` selects the active plan. Reads are deterministically ordered
and bounded; a continuation cursor exposes additional terminal attempts or
historical plan data without expanding the default model context.

`update_plan` uses one stable tagged-union schema. Planning defines the bounded
complete live tree; execution may replace one mutable future subtree without
resending or rewriting unrelated branches changed by workers.

Conceptual input:

```text
UpdatePlanInput:
  reason
  execution_intent
  coordinator_node_id?
  max_concurrency_hint?
  change:
    DefinePlan {
      expected_plan_revision
      root: PlanNodeInput
    }
    ReplaceSubtree {
      target_node_id
      expected_node_revision
      subtree: PlanNodeInput
    }

PlanExecutionIntent:
  continue_planning
  execute_if_authorized
  request_user_review

PlanNodeInput:
  id?                 # required for existing nodes, absent for new nodes
  client_key?         # request-local reference, required for new nodes
  objective
  acceptance[]
  executor_policy
  harness
  recovery_policy
  depends_on[]        # existing node ids or request-local client keys
  children[]
```

Rules:

- `DefinePlan.expected_plan_revision` must match the empty or populated plan
  created by `begin_plan`; `update_plan` never creates a plan implicitly.
- a stale `DefinePlan` returns a compact failed tool outcome with the current
  plan revision and does not partially mutate state;
- `DefinePlan` is valid during `Planning` and replaces the complete mutable live
  planning tree as one transaction.
- `ReplaceSubtree` is valid during `Planning` or `Executing`. Its target must be
  `Pending`, must match `expected_node_revision`, and must have no live lease or
  terminal-result descendant inside the replaced region. Ancestors, active or
  completed nodes, unrelated siblings, and unrelated worker expansions remain
  untouched.
- `ReplaceSubtree` uses node-revision compare-and-swap rather than requiring the
  observed global plan revision to remain unchanged. Unrelated worker progress,
  expansion, or result commits may advance the plan revision without rejecting
  the replacement when the target region and its external dependency contract
  remain unchanged;
- the replacement root must retain `target_node_id`, parent id, and sibling
  position. Existing mutable descendant ids may be retained; omitted mutable
  descendants become `Superseded`, and new descendants use client keys;
- replacement is rejected if superseding a descendant would leave an incoming
  dependency from outside the target subtree unresolved. The coordinator must
  choose a larger mutable target or retain that dependency endpoint;
- Existing live nodes must retain ids returned by earlier snapshots.
- New nodes omit ids, provide a unique request-local `client_key`, and receive
  runtime-owned ids in the output snapshot. They begin as `Pending` with no
  result. Client keys are not persisted.
- Node status, attempts, leases, progress, and results are runtime-owned output
  fields and cannot be authored through `update_plan`.
- Dependencies may reference another new node by client key within the same
  candidate tree; the runtime resolves all client keys before validation and
  commit.
- `execute_if_authorized` enters `Executing` directly when the complete plan
  capability envelope is covered by existing user authorization and no typed
  approval requirement exists. Otherwise the committed plan enters
  `AwaitingApproval` with explicit reasons.
- entering `Executing`, whether directly or after approval, durably records the
  execution-contract fingerprint before any lease can be admitted;
- `request_user_review` always enters `AwaitingApproval`; `continue_planning`
  leaves the plan in `Planning`.
- `continue_planning` is invalid while the plan remains `Executing`.
  `ReplaceSubtree` during execution uses `execute_if_authorized` to remain
  executable or `request_user_review` to pause at an explicit boundary.
- During `Executing`, a change to the established execution-contract
  fingerprint cannot run until user approval resolves the boundary.
- Node completion validates result references before any plan event is
  observable.
- `update_plan` is coordinator-only. Workers never receive it.
- During execution, `update_plan` cannot manufacture attempt outcomes or
  terminal node results. `InProgress`, `Expanded`, `Verifying`, `Completed`,
  and attempt-failure transitions are owned by lease/report/scheduler logic.
- The coordinator may change `max_concurrency_hint` within the runtime ceiling
  without user approval. Runtime/operator configuration remains authoritative.

The provider-facing output remains compact: plan id, new revision, phase,
request-local client-key to runtime-id mappings, changed-root/subtree summary,
scheduler summary, and typed approval requirements. It does not echo the full
plan into the tool continuation. The committed full snapshot is available to
the TUI/events and through explicit bounded `read_plan`; normal context
projection supplies the model with the appropriate current view.

`report_plan_progress` records a non-terminal compact checkpoint or semantic
progress update without resolving the lease. It is valid for a worker lease or
for the coordinator's currently active `Local` attempt lease:

```text
ReportPlanProgressInput:
  lease_id
  expected_node_revision
  summary
  evidence_refs[]
  artifact_refs[]
  next_action?
  checkpoint_ref?
  acknowledged_directive_ids[]
  applied_directive_ids[]
  request_coordinator_review?
```

The report must be bounded and may cite only worker-owned or already promoted
refs. It updates durable progress, directive acknowledgement, and checkpoint
navigation. Heartbeats and provider/tool in-flight state remain runtime-recorded
and do not require model tool calls.

`report_plan_attempt` is the exactly-once local/worker completion boundary:

```text
ReportPlanAttemptInput:
  lease_id
  expected_node_revision
  outcome
  result?
  diagnostic?
  decomposition?
  acknowledged_directive_ids[]
  applied_directive_ids[]

PlanDecompositionInput:
  reason
  children[]
```

Rules:

- the lease must be live and owned by the reporting executor session;
- `completed` requires a valid compact result and durable refs;
- `yielded` requires a durable checkpoint ref and returns the node to pending
  scheduling without consuming a transient-failure retry;
- `decomposed` requires a bounded decomposition with at least one direct child;
  new child ids, attempt resolution, lease resolution, and the node transition
  to `Expanded` commit atomically;
- decomposition children may reference one another by request-local client key
  but cannot contain nested children or rewrite ancestors, siblings, or other
  branches; later workers expand those children lazily;
- every decomposition child harness and workspace scope must stay within the
  authorized execution-contract capability ceiling and the leased node's
  delegable scope;
- a valid decomposition inside the leased subtree commits without coordinator
  review. The runtime does not wake the main coordinator merely because routine
  local expansion occurred;
- a decomposition that changes ancestors, unrelated branches, the established
  root contract, or the permission/capability envelope is rejected without
  resolving the attempt. The worker may revise the decomposition or report a
  boundary blocker that wakes the coordinator/user;
- stale, duplicate, expired, or cross-session reports are rejected without
  changing the plan;
- successful reporting resolves the lease, records the attempt outcome, folds
  the result, updates node state, and wakes the scheduler.

All plan tools are runtime-owned control adapters rather than generic executors
that mutate session state through callbacks. A worker runtime carries a scoped
`PlanWorkerControl` handle containing only its plan, node, attempt, lease,
progress/directive mailbox, and root-session artifact-promotion authority.

### Worker Evidence Promotion

Worker progress or terminal report input may cite child-local
artifacts/evidence. Before committing the progress checkpoint or attempt,
runtime:

```text
validate lease and child-session ownership
-> read referenced child artifacts into owned bounded values
-> release child-session locks
-> stage equivalent root-session artifacts and evidence mappings
-> stage root progress/checkpoint or plan result with promoted refs
-> persist root artifacts and the progress or attempt transaction
-> install state and expose events
```

Cross-session artifact ids are never persisted directly in `PlanNodeResult`.
The report output returns the child-to-root ref mapping. Promotion is bounded by
artifact count and byte limits; oversized exact outputs must use approved
shared-workspace output paths plus compact artifact metadata.

If a worker model loop reaches an ordinary terminal state without a successful
`report_plan_attempt`, the scheduler records a typed `missing_attempt_report`
failure. It never infers completion from free-form final text. Runtime-enforced
cancellation, lease expiry, or process loss instead records the corresponding
typed `cancelled` or `interrupted` outcome without requiring a model report.

## Coordinator-To-Worker Directives

The main coordinator may steer an anomalous or user-selected live attempt
without receiving the worker transcript or controlling routine local decisions.
Routine progress and valid subtree decomposition do not require coordinator
messages.

`control_plan_attempt` submits a persisted attempt-scoped directive:

```text
ControlPlanAttemptInput:
  attempt_id
  expected_lease_id
  expected_node_revision
  kind
  reason
  instruction?
  constraints?
  requested_output[]

CoordinatorDirective:
  directive_id
  sequence
  plan_id
  node_id
  node_revision
  attempt_id
  lease_id
  scope: attempt
  kind
  reason
  instruction?
  constraints
  requested_output[]
  issued_at
  status
  delivered_at?
  acknowledged_at?
  applied_at?

DirectiveKind:
  request_status
  steer
  converge
  checkpoint_and_continue
  checkpoint_and_yield
  cancel_at_safe_point

DirectiveConstraints:
  allow_decomposition?
  require_terminal_report?
  preserve_partial_result
```

Runtime validates that the attempt and lease are still live, persists the
directive as `queued`, and only then acknowledges the coordinator tool call.
Provider and bounded tool calls already in flight are not cancelled merely to
deliver ordinary steering. Typed constraints apply to later runtime admissions
and attempt reports once committed. Before the worker's next provider request,
runtime projects all unresolved directives in sequence order as a high-priority
structured control segment and marks them `delivered`.

The runtime enforces only typed constraints it can prove, such as rejecting a
later decomposition when `allow_decomposition = false`. The worker model
interprets `reason`, `instruction`, and `requested_output`. It reports
`acknowledged_directive_ids` and `applied_directive_ids` through progress or
terminal reports; runtime does not treat prompt projection alone as semantic
acknowledgement.

Directive lifecycle is:

```text
queued -> delivered -> acknowledged -> applied
queued|delivered|acknowledged -> superseded|expired
```

Directives are attempt-scoped by default and expire when that attempt becomes
terminal or interrupted. They are not silently replayed into a new attempt.
Persistent node policy changes, including permanently forbidding further
decomposition, use `update_plan` to revise the node/harness contract instead.

Coordinator intervention follows a graduated path:

```text
request_status
-> steer or converge
-> checkpoint_and_continue or checkpoint_and_yield
-> cancel_at_safe_point
```

A grace review window may wake the coordinator again, but it never cancels an
attempt automatically. Permission violations, invalid side effects, user hard
cancel, lost lease, or consistency threats remain runtime-enforced boundaries
and may stop admission immediately rather than waiting for a directive.

## Stable Tool Contract And Independent Admission

Plan Mode is structured coordination state, not a tool-availability or
permission mode. Activating, revising, approving, pausing, executing, or
completing a plan must not add, remove, rename, rewrite, or reorder the
provider-visible coordinator tools within a session.

The coordinator runtime registers its complete plan-control surface at
construction. Names, descriptions, input schemas, output contracts, and stable
ordering remain identical across `Planning`, `AwaitingApproval`, `Executing`,
and terminal plan phases. A provider-invisible dispatch enum may distinguish
intrinsic plan controls for runtime validation and audit, but it must not drive
phase-dependent request schemas.

General tool calls continue through the existing action policy, permission
admission, and proposal-aware execution path. Plan state neither grants nor
revokes workspace, process, network, trusted-external, permission-escalation, or
other capabilities. If an action was authorized before plan activation, Plan
Mode does not deny it; if it was unauthorized, Plan Mode does not authorize it.
A user instruction such as "plan only" is an explicit task or permission
constraint rather than an implicit property of Plan Mode.

Plan-specific operations still validate actor, phase, plan revision, node
revision, attempt, and lease at execution time. Invalid operations return typed
tool outcomes; hiding a tool is not an authorization mechanism.

Each worker runtime is a separate model execution surface constructed with its
harness-approved general tools and scoped worker plan controls. That tool set
and its ordering remain stable for the entire attempt. Workers do not receive
coordinator plan-authoring controls or direct subagent spawn/cancel tools.
Recursive task expansion flows through the worker report/decomposition contract,
central plan state, and scheduler.

Request construction keeps static system instructions and stable tool
definitions ahead of dynamic plan projection when the provider-neutral context
contract permits it. Correctness never depends on cache reuse, but plan phase
changes must not cause avoidable prompt/KV cache invalidation.

## Context Projection

Plan context is structured control-plane state. It is not appended as ordinary
chat history and does not replace exact evidence. The same compiler produces
coordinator and worker projections from persisted plan state.

Request compilation keeps stable system instructions and tool definitions
unchanged, then adds the dynamic plan segment after Task Anchor and before
ordinary compiled context:

### Planning Or Awaiting Approval

Project a bounded full outline so the model can revise it:

```text
plan phase and revision
root objective and acceptance
recursive live-node outline
node ids, statuses, dependencies, and executor hints
```

### Executing Coordinator

Project:

```text
plan phase and revision
root objective and acceptance
current coordination path
recent completed/failed attempt summaries that require synthesis or replanning
active attempt progress snapshots that requested or require review
queued/delivered directive status for coordinator-owned interventions
blocked nodes and boundary reasons
explicit evidence/artifact refs
```

The coordinator does not inherit worker transcripts. It receives compact
attempt results and may read exact refs when needed.

When the coordinator holds a `Local` attempt lease, the same projection also
includes that node's full contract, local lease/attempt ids, relevant dependency
results, and the scoped progress/terminal-report contract. The stable
coordinator tool schema does not change; report tools are rejected when no
matching local lease exists.

### Executing Worker

Project only:

```text
plan phase and revision
root objective and acceptance
root-to-active-node ancestor path
current node full contract
compact results from explicit dependencies
explicit evidence/artifact refs
lease id, attempt id, heartbeat/liveness contract, and scoped
progress/result/decomposition contract
unresolved coordinator directives in sequence order
```

Unrelated sibling branches and their transcripts are omitted. The active
worker's recent transcript/tool continuity remains governed by its child
session and checkpoint rules.

Plan projection must be deterministic from persisted `PlanState` and artifact
state. Role/profile defaults may influence future evidence selection but cannot
broaden the explicit plan projection.

## Central Plan Scheduler

Each executing plan owns one runtime scheduler. It is a deterministic state
machine over persisted plan state, not a model loop. It derives admission and
recovery commands but does not mutate `PlanState` outside `PlanController`.

### Ready-Leaf Derivation

A node is schedulable when:

- plan phase is `Executing`;
- it is either a `Pending` leaf with no live non-superseded children, or a
  `Verifying` expanded node whose children are all terminal;
- every dependency is completed with a durable result;
- no ancestor is blocked, failed, cancelled, or superseded;
- no live lease exists for the same node revision;
- its write scope and exclusive capabilities do not conflict with live leases;
- runtime concurrency, provider quota, and operational admission have capacity.

An unknown or local-workspace-effect `CommandExec` capability is plan-exclusive
unless the runtime can prove the exact action is read-only or scratch-only.
Process tooling must not bypass declared workspace write scopes.

The ready set is derived in stable tree/order/id order so identical persisted
state produces identical admission decisions.

### Lease Reservation

Reservation is transactional:

```text
derive candidate ready leaf
-> submit reservation command to PlanController
-> validate current revision and capacity in controller order
-> allocate attempt and lease ids
-> persist reserved attempt/lease state
-> construct worker runtime from the exact harness snapshot
-> emit PlanLeaseStarted
```

If worker construction fails, the attempt becomes a typed infrastructure
failure and follows recovery policy. A lease is never silently discarded.

### Worker Construction

Refactor the existing `ChildRuntimeFactory` boundary so the scheduler can build
a worker from:

```text
session id
plan id / node id / node revision
ancestor path projection
dependency results and evidence refs
scoped attempt-report/decomposition handle
scoped progress/directive mailbox
harness/model generation config
workspace scope
cancellation/liveness heartbeat
```

Worker construction preserves the existing conservative process boundary: a
worker with scoped workspace writes does not receive an unrestricted local
workspace-effect process lane. Read-only commands remain available when
classified; broader process effects require an explicit exclusive harness and
cannot overlap conflicting leases.

All worker runtimes are depth-one executors relative to the root scheduler.
Task descendants may reach depth 16 because a worker expands central plan state
and releases its lease; the scheduler then starts new depth-one workers for the
new leaves. This is intentional and avoids an opaque process hierarchy.

### Local, Delegate, And Auto

- `Delegate` always uses a child worker when admitted.
- `Local` queues the node for the main coordinator execution lane.
- `Auto` prefers child execution when a worker slot is available and the
  harness is child-safe; otherwise it may use the local lane.

Only one local node may run at a time. Child workers may run concurrently up to
the minimum of runtime `max_threads`, the coordinator's current concurrency
hint, and currently available provider/runtime capacity.

### Coordinator Wake-Ups

The scheduler queues an internal coordinator continuation when:

- a semantic failure needs replanning;
- an expanded parent becomes ready for synthesis or verification;
- all currently runnable work is blocked;
- a worker requests a boundary change;
- a worker explicitly requests coordinator review;
- adaptive progress policy requests semantic review;
- root completion or final acceptance must be decided.

Coordinator wake-ups use compact attempt results and exact refs, never worker
transcripts. They serialize through the existing main interactive run boundary.
User `next` input remains higher priority and may pause new lease admission when
it changes the established execution boundary. Valid routine worker
decomposition does not wake the coordinator.

### Adaptive Progress Review

Elapsed time alone never fails or cancels a node. Runtime records a bounded
progress snapshot:

```text
PlanAttemptProgress:
  elapsed
  model_turns
  reported_usage
  last_worker_heartbeat
  last_runtime_activity
  last_durable_progress
  provider_request_in_flight
  tool_call_in_flight
  artifacts_created
  changed_paths
  acceptance_evidence
  repeated_failure_fingerprint?
```

Signals have different meanings:

- worker heartbeats prove liveness, including while a slow provider request is
  in flight;
- stream/tool activity proves activity but not semantic progress;
- durable artifacts, results, plan revisions, and acceptance evidence prove
  resumable progress;
- repeated identical failures or a long window with no durable progress request
  coordinator review.

`checkpoint_turn_interval` requests a compact handoff/checkpoint at a safe
model boundary. It does not terminate the node. A provider request or bounded
tool call already in flight is not cancelled because a review point was
reached.

When progress is ambiguous, runtime emits `PlanProgressReviewRequested` and
the coordinator chooses:

```text
continue
request_status
steer or converge
checkpoint_and_continue
checkpoint_and_yield
replan
block
```

Status, steering, checkpoint, yield, and safe cancellation choices use
`control_plan_attempt`. Replanning uses `update_plan` against a mutable future
subtree. Runtime never infers a semantic instruction from elapsed time alone.

Continued durable progress may be pre-authorized for automatic continuation.
No user approval is required merely because execution is long. The user is
involved only when objective, acceptance, permission/capability, destructive
authority, or required external input changes.

### Result Folding And Parent Progress

When an attempt completes:

```text
commit exact refs
-> commit attempt outcome
-> resolve lease
-> update node status/result
-> recompute ready descendants and dependents
-> move expanded parents to Verifying when children are terminal
-> wake coordinator when synthesis, replanning, or approval is required
-> admit the next deterministic ready set
```

Parent completion is never inferred solely from child completion. An expanded
parent receives a verification/synthesis attempt or an explicit compact result
before becoming `Completed`.

## Typed Recovery

Recovery operates on attempts, not by erasing node history.

### Transient Failure

Provider/infrastructure failure may retry when:

- the node recovery policy still permits retry after this failure class;
- the attempt has no committed result;
- no observable external side effect was committed, unless the action is
  explicitly idempotent under its executor contract.

The failed attempt and diagnostic remain durable. A retry creates a new attempt
and lease after configured backoff.

### Semantic Failure

A worker may report a semantic failure with evidence. The node becomes
`Failed`, the result folds into coordinator context, and automatic scheduling
of dependent nodes stops. The coordinator may revise only mutable future
subtrees or block when the established boundary must change.

### Boundary Failure

Initial execution does not require a second approval when existing user
authorization covers the complete plan. Later requests to change the
established root objective, root acceptance, permission/capability ceiling, or
destructive external authority populate `approval_requirements` and transition
the plan to `AwaitingApproval` or `Blocked` when required external input is
missing. An explicit user or governing-skill review request does the same even
without permission expansion. Long elapsed time alone is not an approval
boundary.

### Cancellation, Expiry, And Resume

- cancellation requests the worker token and prevents new side effects;
- an expired lease records an `interrupted` attempt before requeue decisions;
- process restart treats persisted live leases as interrupted because their
  executor tasks no longer exist;
- attempt-scoped directives for an interrupted or terminal attempt become
  `expired` and are not projected into its successor;
- a `yielded` attempt requeues the node from its durable checkpoint without
  counting as a transient failure;
- completed results are never rerun;
- an interrupted node is requeued only when recovery policy and side-effect
  evidence allow it;
- otherwise the node becomes blocked for coordinator/user review.

## Events And Durability

Add provider-neutral journal/public events:

```text
PlanUpdated
PlanPhaseChanged
PlanNodeReady
PlanLeaseStarted
PlanProgressUpdated
PlanProgressReviewRequested
PlanAttemptProgressReported
PlanDirectiveUpdated
PlanAttemptFinished
```

`PlanUpdated` includes the bounded current snapshot and a compact revision
summary. This duplicates some bounded state in the event stream but keeps the
first TUI projector deterministic and avoids a second UI-side query protocol.
Usage can later justify operation events plus snapshots.

Required commit order:

```text
receive the next PlanController command
-> validate actor, revisions, lease, permissions, and candidate invariants
-> validate referenced artifacts/evidence
-> stage candidate plan/control state and events without installing them
-> persist the staged resume-safe savepoint when configured
-> install the staged plan state in memory
-> reply to the command caller
-> expose committed plan/directive/progress events
-> derive scheduler follow-up commands
```

No observable event may claim a plan revision, directive transition, progress
checkpoint, node result, lease transition, or phase transition before the
corresponding state is durably committed.
If staged persistence fails, the current in-memory plan and revision remain
unchanged.

For worker progress and terminal results, exact artifacts and evidence commit
before progress/directive acknowledgement, attempt or node transition,
dependency release, scheduler wake-up, and public event.

Session persistence advances to a new format version with backward-compatible
loading of the current version as `plan = None`.

## Interactive Runtime Controls

Extend the interactive handle with typed commands:

```text
enter_plan_mode
approve_plan
revise_plan
cancel_plan
pause_plan_scheduling
resume_plan_scheduling
```

Rules:

- commands are accepted only at a safe interactive boundary;
- entering Plan Mode creates `Planning` state if needed but is optional because
  the coordinator may call `begin_plan`;
- plans covered by existing authorization may enter `Executing` without this
  control;
- approval requires `AwaitingApproval`, a valid non-empty plan, and resolution
  of every typed approval requirement;
- the user's approve action resolves review-only requirements. Permission,
  destructive-authority, and external-input requirements must already carry
  valid runtime-owned resolution refs and are revalidated before the phase
  changes;
- rejection keeps the plan non-executable and returns it to `Planning` or
  `Blocked` with the user's reason;
- revision from `AwaitingApproval` returns to `Planning`;
- revision from `Executing` pauses plan execution and requires the main run to
  be idle first;
- pause stops new lease admission but does not silently cancel live workers;
- resume re-derives the deterministic ready set;
- cancellation cancels live leases cooperatively, preserves the final plan and
  attempt snapshots, and changes phase to `Cancelled` after outcomes settle.

## TUI Design

### State Ownership

`TuiState` owns only UI projection state:

- latest `PlanSnapshot`;
- selected node id;
- collapsed node ids;
- plan scroll offset;
- plan view open/closed state;
- node inspector open/closed state;
- latest bounded lease/attempt views for displayed nodes.
- latest bounded progress and directive views for displayed attempts.

Runtime owns plan truth, revisions, phases, results, directives, and approval
requirements.

### Command Palette

Add phase-sensitive commands:

- `Enter Plan Mode`
- `Approve plan and execute` when approval requirements exist
- `Revise plan`
- `Open/close plan`
- `Pause/resume plan scheduling`
- `Retry selected interrupted node`
- `Cancel plan`

Do not add permanent default key bindings without usage evidence. The command
palette is the first discoverable control surface; plan navigation keys apply
only while the plan pane/overlay has focus.

### Layout

- At 80 columns and above, render timeline plus a bounded right plan pane when
  a plan exists or Plan Mode is active.
- Below 80 columns, the plan opens as a full-screen overlay.
- The active path is automatically revealed, but runtime activity must not
  reset the user's selected node, manual folds, or scroll position.
- The plan pane displays progress, phase, recursive nodes, status symbols, the
  active selection, live worker count, queued-ready count, and blocked count.
- Timeline entries summarize plan creation, direct authorization, expansion,
  revision, required/received approval, steering, blocking, completion, and
  cancellation.
- Enter on a selected node opens a read-only inspector with objective,
  acceptance, dependencies, executor/harness contract, live lease, attempt
  history, progress, queued/delivered directives, result, approval reasons, and
  revision metadata.
- Long-running nodes show elapsed time, last durable progress, heartbeat, and
  in-flight provider/tool state. The UI does not present a default countdown
  implying that the node will be killed at a soft review point.

Status rendering must use both symbols and semantic colors:

```text
pending      o
ready        diamond
in progress  filled diamond
completed    check
blocked      !
failed       x
superseded   ~
```

Use ASCII-compatible fallback symbols where terminal capability or existing
rendering conventions require them.

### Module Structure

Do not add plan rendering to the already large general renderer. Prefer focused
modules such as:

```text
crates/merry-cli/src/tui/plan.rs
crates/merry-cli/src/tui/plan_render.rs
```

Exact names may follow surrounding module conventions, but plan projection,
navigation, and rendering should remain separate from provider/settings UI.

## Error Handling

Add typed plan validation/runtime errors for:

- no active plan;
- wrong phase;
- stale revision;
- stale subtree target or node revision;
- invalid root/tree/dependency topology;
- node/depth/collection/text limits;
- unknown or duplicate node ids;
- illegal status transition;
- immutable active/completed node rewrite;
- worker mutation outside its leased subtree;
- duplicate, stale, expired, or cross-session attempt report;
- invalid progress/checkpoint refs or directive acknowledgement;
- directive targeting a terminal, interrupted, or different leased attempt;
- invalid directive lifecycle transition;
- concurrent write-scope or exclusive-capability conflict;
- scheduler capacity, provider quota, or operator resource constraint;
- invalid harness or recovery policy;
- unsafe automatic retry after committed side effects;
- established execution-contract fingerprint change;
- missing evidence/artifact refs;
- approval without a valid plan or unresolved approval requirements;
- closed or unavailable PlanController command channel;
- interactive phase command attempted while a run is active.

Provider-facing plan tool failures return compact JSON outcomes and resolve the
tool call durably. Infrastructure and persistence failures remain typed runtime
errors and must not partially commit the candidate plan.

## Testing

### Core And Runtime Unit Tests

```text
plan_tree_rejects_parent_cycles
plan_tree_rejects_dependency_cycles
plan_tree_rejects_descendant_dependency
plan_tree_assigns_stable_runtime_node_ids
plan_define_rejects_stale_plan_revision_without_mutation
plan_define_replaces_complete_mutable_planning_tree
plan_replace_subtree_rejects_stale_node_revision
plan_replace_subtree_allows_unrelated_global_revision_advance
plan_replace_subtree_preserves_unrelated_worker_expansion
plan_replace_subtree_rejects_active_or_completed_target
plan_completion_requires_existing_evidence_refs
plan_execution_contract_change_requires_reapproval
plan_context_projects_active_path_not_unrelated_siblings
worker_decomposition_is_confined_to_leased_node
worker_decomposition_cannot_expand_capability_scope
valid_worker_decomposition_requires_no_coordinator_review
plan_ready_set_is_deterministic
parallel_ready_leaves_reject_overlapping_write_scopes
plan_attempt_report_is_exactly_once
attempt_contains_multiple_model_turns_and_tool_calls
checkpoint_and_continue_preserves_attempt_id
yield_or_interruption_requires_new_attempt_id
attempt_scoped_directive_expires_when_attempt_ends
directive_lifecycle_requires_explicit_worker_acknowledgement
directive_and_progress_payload_limits_are_enforced
worker_report_promotes_exact_artifacts_to_root_session
```

### Runtime Integration Tests

```text
begin_plan_is_available_before_plan_exists
coordinator_tool_specs_and_order_stay_stable_across_plan_phases
plan_activation_does_not_change_general_action_admission
authorized_workspace_action_remains_admitted_during_planning
unauthorized_workspace_action_remains_denied_during_planning
worker_tool_specs_stay_stable_for_attempt
dynamic_plan_context_follows_stable_request_prefix
update_plan_tool_result_is_compact_and_does_not_echo_full_tree
authorized_plan_enters_execution_without_second_approval
explicit_review_request_enters_awaiting_approval
capability_expansion_enters_awaiting_approval
interactive_approval_resolves_typed_requirements
execution_contract_keeps_same_ids_and_revision_history
plan_controller_serializes_concurrent_worker_reports_without_lost_updates
user_pause_prevents_new_lease_after_commit_while_accepting_terminal_reports
plan_events_follow_durable_state_commit
plan_round_trips_through_session_store
legacy_session_loads_without_plan
plan_scheduler_starts_disjoint_ready_leaves_concurrently
worker_can_expand_leased_node_without_spawning_child_agent
valid_worker_expansion_does_not_wake_coordinator
expanded_descendants_are_scheduled_by_root_scheduler
completed_results_unlock_dependent_leaf
transient_attempt_retries_with_new_attempt_id
semantic_failure_wakes_coordinator_for_replan
directive_waits_for_safe_boundary_during_inflight_provider_request
converge_directive_can_forbid_current_attempt_decomposition
worker_progress_acknowledges_and_applies_directive
attempt_terminal_state_expires_unapplied_directives
grace_review_wakes_coordinator_without_auto_cancelling_worker
long_running_worker_heartbeats_renew_lease
elapsed_time_alone_does_not_cancel_progressing_attempt
slow_inflight_provider_request_defers_progress_review
no_durable_progress_wakes_coordinator_without_user_budget_prompt
resume_interrupts_stale_leases_without_replaying_completed_nodes
cancelling_plan_stops_new_leases_and_cancels_live_workers
worker_without_attempt_report_fails_instead_of_completing
```

### TUI Tests

```text
plan_palette_commands_follow_runtime_phase
wide_tui_renders_timeline_and_recursive_plan_without_overlap
narrow_tui_renders_full_screen_plan_overlay
plan_event_updates_tree_without_resetting_selection
active_path_is_revealed_without_unfolding_unrelated_branches
plan_node_inspector_renders_bounded_content
plan_pane_renders_live_workers_ready_queue_and_blocked_counts
plan_inspector_renders_approval_requirements_and_directive_status
pause_resume_scheduler_commands_follow_runtime_state
long_running_node_renders_progress_without_fake_deadline
```

Render-buffer acceptance must cover at least 50x20, 80x24, and 140x40.

### Full Verification

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
uv run --with pytest python -m pytest tests -q
```

## Implementation Order Constraint

Implementation should proceed as one complete vertical capability in this
order:

1. Core plan ids/snapshots/events and tree validation.
2. Root session state, `PlanController` command serialization, transaction
   persistence, and resume migration.
3. Stable coordinator/worker plan tools, tagged `DefinePlan`/`ReplaceSubtree`
   updates, authorization transitions, and cache-aware request composition.
4. Attempts, leases, progress checkpoints, coordinator directives, and
   coordinator/worker context projection.
5. Central ready-leaf scheduler, child-runtime factory integration, autonomous
   scoped worker decomposition, dependency release, parent verification, and
   typed recovery.
6. Interactive approval/pause/resume/cancel controls and direct execution under
   existing authorization.
7. TUI projection, responsive tree, inspector, worker/directive state, approval
   requirements, and timeline events.
8. End-to-end fake-provider acceptance covering concurrent grandchildren,
   direct authorization, steering, replanning, retry, cancellation, resume, and
   full verification.

Do not substitute a display-only plan tree or model-driven `spawn/wait` loop for
the scheduler acceptance target. The first usage milestone must execute,
steer, and recover a recursively expanded concurrent plan end to end.

## Delivery Focus

This user-requested feature is the active delivery focus for this implementation
round. It does not update `ROADMAP.md` priority ordering. Completion evidence is
the offline concurrent recursive-plan acceptance workflow, typed recovery and
persistence/resume coverage, TUI buffer coverage, and the repository
verification commands.
