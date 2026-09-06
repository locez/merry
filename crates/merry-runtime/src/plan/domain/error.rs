use merry_core::{PlanId, PlanNodeId, PlanNodeStatus, PlanPhase};
use thiserror::Error;

/// Validation, authorization, and lifecycle failures in the durable Plan domain.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    #[error("authored node status {status:?} is runtime-owned")]
    InvalidAuthoredNodeStatus { status: PlanNodeStatus },
    #[error("subagent scope violation: {reason}")]
    SubagentScopeViolation { reason: &'static str },
    #[error("plan update reason {reason}")]
    InvalidText {
        field: &'static str,
        reason: &'static str,
    },
    #[error("plan phase {actual:?} does not allow {operation}")]
    WrongPhase {
        actual: PlanPhase,
        operation: &'static str,
    },
    #[error("plan revision is stale: expected {expected}, actual {actual}")]
    StalePlanRevision { expected: u64, actual: u64 },
    #[error("plan identity is stale: expected {expected}, actual {actual}")]
    StalePlanIdentity { expected: PlanId, actual: PlanId },
    #[error("node {node_id} revision is stale: expected {expected}, actual {actual}")]
    StaleNodeRevision {
        node_id: PlanNodeId,
        expected: u64,
        actual: u64,
    },
    #[error("plan must contain exactly one root")]
    RootMissing,
    #[error("plan root must not have a parent")]
    RootHasParent,
    #[error("plan node {node_id} is missing a parent")]
    NodeMissingParent { node_id: PlanNodeId },
    #[error("plan node {node_id} references unknown parent {parent_id}")]
    UnknownParent {
        node_id: PlanNodeId,
        parent_id: PlanNodeId,
    },
    #[error("plan node {node_id} is not reachable from the root")]
    UnreachableNode { node_id: PlanNodeId },
    #[error("plan parent graph contains a cycle")]
    ParentCycle,
    #[error("plan dependency graph contains a cycle")]
    DependencyCycle,
    #[error("plan node {node_id} depends on itself")]
    SelfDependency { node_id: PlanNodeId },
    #[error("plan node {node_id} depends on descendant {dependency_id}")]
    DependsOnDescendant {
        node_id: PlanNodeId,
        dependency_id: PlanNodeId,
    },
    #[error("unknown plan node {node_id}")]
    UnknownNode { node_id: PlanNodeId },
    #[error("unknown dependency node {node_id}")]
    UnknownDependency { node_id: PlanNodeId },
    #[error("unknown request-local client key {client_key}")]
    UnknownClientKey { client_key: String },
    #[error("duplicate request-local client key {client_key}")]
    DuplicateClientKey { client_key: String },
    #[error("duplicate plan node id {node_id}")]
    DuplicateNodeId { node_id: PlanNodeId },
    #[error("new plan node must provide exactly one client_key and no id")]
    InvalidNewNodeIdentity,
    #[error("existing plan node must provide an id and no client_key")]
    InvalidExistingNodeIdentity,
    #[error("plan has {actual} live nodes, maximum is {maximum}")]
    TooManyNodes { actual: usize, maximum: usize },
    #[error("plan node {node_id} has {actual} children, maximum is {maximum}")]
    TooManyChildren {
        node_id: PlanNodeId,
        actual: usize,
        maximum: usize,
    },
    #[error("plan depth {actual} exceeds maximum {maximum}")]
    PlanTooDeep { actual: usize, maximum: usize },
    #[error("node has {actual} dependencies, maximum is {maximum}")]
    TooManyDependencies { actual: usize, maximum: usize },
    #[error("node has {actual} acceptance items, maximum is {maximum}")]
    TooManyAcceptanceItems { actual: usize, maximum: usize },
    #[error("{field} has {actual} items, maximum is {maximum}")]
    TooManyPayloadItems {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("node recovery policy allows {actual} transient attempts, maximum is {maximum}")]
    TooManyTransientAttempts { actual: u8, maximum: u8 },
    #[error("attempt {attempt_id} has {actual} non-terminal directives, maximum is {maximum}")]
    TooManyActiveDirectives {
        attempt_id: merry_core::PlanAttemptId,
        actual: usize,
        maximum: usize,
    },
    #[error("serialized plan snapshot is {actual} bytes, maximum is {maximum}")]
    SnapshotTooLarge { actual: usize, maximum: usize },
    #[error("duplicate sibling order {sibling_order} below parent {parent_id:?}")]
    DuplicateSiblingOrder {
        parent_id: Option<PlanNodeId>,
        sibling_order: u16,
    },
    #[error("node {node_id} scope path is invalid: {path}")]
    InvalidScopePath { node_id: PlanNodeId, path: String },
    #[error("node {node_id} exceeds its parent or authorized capability envelope")]
    CapabilityEnvelopeExceeded { node_id: PlanNodeId },
    #[error("node {node_id} is not mutable while in status {status:?}")]
    NodeNotMutable {
        node_id: PlanNodeId,
        status: PlanNodeStatus,
    },
    #[error(
        "plan node {node_id} or its subtree is owned by an active linked subagent; wait for a terminal result or cancel it and create a new assignment"
    )]
    ActiveSubagentOwnsSubtree { node_id: PlanNodeId },
    #[error("replacement root must retain target node id {target_node_id}")]
    ReplacementRootIdentity { target_node_id: PlanNodeId },
    #[error("subtree replacement would leave incoming dependency on superseded node {node_id}")]
    IncomingDependencyWouldDangle { node_id: PlanNodeId },
    #[error("max_concurrency_hint must be between one and runtime maximum {maximum}")]
    InvalidConcurrencyHint { maximum: usize },
    #[error("persisted plan id counters must be non-zero")]
    InvalidPersistedCounters,
    #[error("plan has no root node")]
    EmptyPlan,
    #[error("plan approval requirement {requirement_id} has no valid runtime resolution")]
    UnresolvedApprovalRequirement {
        requirement_id: merry_core::PlanApprovalRequirementId,
    },
    #[error("active plan attempts prevent {operation}")]
    ActiveAttemptsPreventControl { operation: &'static str },
    #[error("node {node_id} is not ready for execution")]
    NodeNotReady { node_id: PlanNodeId },
    #[error("node {node_id} already has a live lease")]
    LiveLeaseExists { node_id: PlanNodeId },
    #[error("node {node_id} has no blocked interrupted attempt eligible for explicit retry")]
    InterruptedRetryUnavailable { node_id: PlanNodeId },
    #[error("plan lease {lease_id} was not found")]
    UnknownLease { lease_id: merry_core::PlanLeaseId },
    #[error("plan lease {lease_id} is not live")]
    LeaseNotLive { lease_id: merry_core::PlanLeaseId },
    #[error("plan attempt {attempt_id} was not found")]
    UnknownAttempt {
        attempt_id: merry_core::PlanAttemptId,
    },
    #[error("executor session {executor_session_id} has no active plan attempt")]
    NoActiveAttemptForExecutor {
        executor_session_id: merry_core::SessionId,
    },
    #[error("executor session {executor_session_id} already has an active plan attempt")]
    ActiveAttemptExistsForExecutor {
        executor_session_id: merry_core::SessionId,
    },
    #[error("executor session {executor_session_id} has multiple active plan attempts")]
    MultipleActiveAttemptsForExecutor {
        executor_session_id: merry_core::SessionId,
    },
    #[error("plan attempt {attempt_id} belongs to another executor session")]
    AttemptOwnershipMismatch {
        attempt_id: merry_core::PlanAttemptId,
    },
    #[error("plan attempt {attempt_id} is already resolved")]
    AttemptAlreadyResolved {
        attempt_id: merry_core::PlanAttemptId,
    },
    #[error("attempt lease node revision is stale: expected {expected}, actual {actual}")]
    AttemptNodeRevisionMismatch { expected: u64, actual: u64 },
    #[error("attempt outcome {outcome:?} has an invalid result/decomposition contract")]
    InvalidAttemptOutcome {
        outcome: merry_core::PlanAttemptOutcome,
    },
    #[error("attempt decomposition must contain at least one direct child")]
    EmptyDecomposition,
    #[error("attempt decomposition children must be direct leaves")]
    NestedDecomposition,
    #[error(
        "authored plan input may contain only one root and its direct children; deeper work belongs to the linked child scope"
    )]
    NestedPlanInput,
    #[error("directive {directive_id} was not found for the current attempt")]
    UnknownDirective {
        directive_id: merry_core::PlanDirectiveId,
    },
    #[error("directive {directive_id} cannot transition from {status:?} to {target}")]
    InvalidDirectiveTransition {
        directive_id: merry_core::PlanDirectiveId,
        status: merry_core::PlanDirectiveStatus,
        target: &'static str,
    },
    #[error("directive target attempt or lease is stale")]
    StaleDirectiveTarget,
    #[error("plan result references missing artifact {artifact_id}")]
    MissingArtifactRef { artifact_id: merry_core::ArtifactId },
    #[error("plan result references invalid evidence in artifact {artifact_id}")]
    InvalidEvidenceRef { artifact_id: merry_core::ArtifactId },
    #[error("promoted plan artifact {artifact_id} conflicts with existing root-session content")]
    ArtifactPromotionConflict { artifact_id: merry_core::ArtifactId },
}
