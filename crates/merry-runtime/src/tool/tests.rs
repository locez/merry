use super::{
    ActionProposal, ActionProposalError, ActionProposalEvidence, RegisteredTool, ToolActionKind,
    ToolConcurrency, ToolExecutionContext, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
    ToolRunner, WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
};
use crate::{ProcessActionIntent, ProcessEnvPolicy};
use merry_core::{
    PendingToolCall, ToolCallArguments, ToolCallId, ToolInputSchema, ToolName, ToolSpec,
};
use schemars::Schema;
use serde_json::json;
use std::sync::Arc;

struct StaticToolExecutor;

impl ToolExecutor for StaticToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async { Ok(ToolExecutionOutcome::succeeded_text("ok")) })
    }
}

fn tool_spec(name: &str) -> ToolSpec {
    let schema =
        Schema::try_from(json!({ "type": "object" })).expect("test schema should be a JSON schema");
    ToolSpec::new(
        ToolName::new(name).expect("valid tool name"),
        "Test tool",
        ToolInputSchema::new(schema).expect("valid tool schema"),
    )
    .expect("valid tool spec")
}

fn pending_call(name: &str) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new("call-proposal").expect("valid call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolCallArguments::new(Default::default()),
    )
}

#[test]
fn read_only_constructor_classifies_tool_as_read_only() {
    let tool = RegisteredTool::read_only(tool_spec("read_only_tool"), Arc::new(StaticToolExecutor));

    assert_eq!(tool.action_kind(), ToolActionKind::ReadOnly);
}

#[test]
fn registered_tool_defaults_to_runtime_runner() {
    let tool = RegisteredTool::read_only(tool_spec("lookup_order"), Arc::new(StaticToolExecutor));

    assert_eq!(tool.runner(), ToolRunner::Runtime);
}

#[test]
fn bridge_tool_carries_spec_without_runtime_executor() {
    let tool = RegisteredTool::bridge(tool_spec("lookup_order"));

    assert_eq!(tool.runner(), ToolRunner::Bridge);
    assert_eq!(tool.spec().name().as_str(), "lookup_order");
}

#[test]
fn registered_tools_default_to_exclusive_execution() {
    let explicit = RegisteredTool::new(
        tool_spec("write_tool"),
        Arc::new(StaticToolExecutor),
        ToolActionKind::WorkspaceWrite,
    );
    let read_only = RegisteredTool::read_only(tool_spec("read_tool"), Arc::new(StaticToolExecutor));
    let bridge = RegisteredTool::bridge(tool_spec("bridge_tool"));

    assert_eq!(explicit.concurrency(), ToolConcurrency::Exclusive);
    assert_eq!(read_only.concurrency(), ToolConcurrency::Exclusive);
    assert_eq!(bridge.concurrency(), ToolConcurrency::Exclusive);
}

#[test]
fn parallel_safe_opt_in_changes_only_runtime_metadata() {
    let tool = RegisteredTool::read_only(tool_spec("lookup_order"), Arc::new(StaticToolExecutor));
    let visible_spec = serde_json::to_value(tool.spec()).expect("tool spec should serialize");
    let tool = tool.with_parallel_safe_execution();

    assert_eq!(tool.concurrency(), ToolConcurrency::ParallelSafe);
    assert_eq!(
        serde_json::to_value(tool.spec()).expect("tool spec should serialize"),
        visible_spec
    );
}

#[test]
fn explicit_constructor_preserves_non_read_action_kind() {
    let tool = RegisteredTool::new(
        tool_spec("write_tool"),
        Arc::new(StaticToolExecutor),
        ToolActionKind::WorkspaceWrite,
    );

    assert_eq!(tool.action_kind(), ToolActionKind::WorkspaceWrite);
    assert!(!tool.proposals_enabled());
}

#[test]
fn action_proposal_opt_in_marks_registered_tool() {
    let tool = RegisteredTool::new(
        tool_spec("write_tool"),
        Arc::new(StaticToolExecutor),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();

    assert_eq!(tool.action_kind(), ToolActionKind::WorkspaceWrite);
    assert!(tool.proposals_enabled());
}

#[test]
fn apply_patch_proposal_validates_relative_path_and_sizes() {
    let proposal = WorkspacePatchProposal::new(
        "dir/note.txt",
        3,
        5,
        11,
        13,
        "fnv1a64:0123456789abcdef",
        "fnv1a64:fedcba9876543210",
    )
    .expect("valid workspace patch proposal");

    assert_eq!(proposal.relative_path(), "dir/note.txt");
    assert_eq!(proposal.preimage_bytes(), 3);
    assert_eq!(proposal.replacement_bytes(), 5);
    assert_eq!(proposal.file_bytes_before(), 11);
    assert_eq!(proposal.file_bytes_after(), 13);
    assert_eq!(
        proposal.file_fingerprint_before(),
        "fnv1a64:0123456789abcdef"
    );
    assert_eq!(
        proposal.file_fingerprint_after(),
        "fnv1a64:fedcba9876543210"
    );

    let new_file = WorkspacePatchProposal::new(
        "new.txt",
        0,
        5,
        0,
        5,
        "fnv1a64:0123456789abcdef",
        "fnv1a64:fedcba9876543210",
    )
    .expect("new-file workspace patch proposal should allow an empty preimage");
    assert_eq!(new_file.preimage_bytes(), 0);
    assert_eq!(new_file.file_bytes_before(), 0);
    assert_eq!(new_file.file_bytes_after(), 5);

    let absolute = WorkspacePatchProposal::new(
        "/tmp/note.txt",
        3,
        5,
        11,
        13,
        "fnv1a64:0123456789abcdef",
        "fnv1a64:fedcba9876543210",
    )
    .expect_err("absolute paths are rejected");
    assert!(matches!(
        absolute,
        ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            ..
        }
    ));

    let dot_segment = WorkspacePatchProposal::new(
        "dir/../note.txt",
        3,
        5,
        11,
        13,
        "fnv1a64:0123456789abcdef",
        "fnv1a64:fedcba9876543210",
    )
    .expect_err("dot segments are rejected");
    assert!(matches!(
        dot_segment,
        ActionProposalError::InvalidWorkspacePatch {
            field: "relative_path",
            ..
        }
    ));

    let mismatched = WorkspacePatchProposal::new(
        "dir/note.txt",
        3,
        5,
        11,
        99,
        "fnv1a64:0123456789abcdef",
        "fnv1a64:fedcba9876543210",
    )
    .expect_err("projected size must match patch sizes");
    assert!(matches!(
        mismatched,
        ActionProposalError::InvalidWorkspacePatch {
            field: "file_bytes_after",
            ..
        }
    ));
}

#[test]
fn apply_patch_execution_evidence_validates_counts_and_fingerprints() {
    let evidence = WorkspacePatchExecutionEvidence::new(
        "dir/note.txt",
        3,
        5,
        11,
        13,
        "fnv1a64:0123456789abcdef",
        "fnv1a64:fedcba9876543210",
    )
    .expect("valid workspace patch execution evidence");

    assert_eq!(evidence.relative_path(), "dir/note.txt");
    assert_eq!(evidence.preimage_bytes(), 3);
    assert_eq!(evidence.replacement_bytes(), 5);
    assert_eq!(evidence.file_bytes_before(), 11);
    assert_eq!(evidence.file_bytes_after(), 13);
    assert_eq!(
        evidence.file_fingerprint_before(),
        "fnv1a64:0123456789abcdef"
    );
    assert_eq!(
        evidence.file_fingerprint_after(),
        "fnv1a64:fedcba9876543210"
    );

    let invalid = WorkspacePatchExecutionEvidence::new(
        "dir/note.txt",
        3,
        5,
        11,
        13,
        "sha256:not-accepted",
        "fnv1a64:fedcba9876543210",
    )
    .expect_err("fingerprints use the explicit non-cryptographic prefix");
    assert!(matches!(
        invalid,
        ActionProposalError::InvalidWorkspacePatch {
            field: "file_fingerprint_before",
            ..
        }
    ));
}

#[test]
fn action_proposal_rejects_read_only_and_blank_text() {
    let call = pending_call("apply_patch");
    let evidence = ActionProposalEvidence::WorkspacePatch(
        WorkspacePatchProposal::new(
            "note.txt",
            3,
            5,
            11,
            13,
            "fnv1a64:0123456789abcdef",
            "fnv1a64:fedcba9876543210",
        )
        .expect("valid workspace patch proposal"),
    );

    let read_only = ActionProposal::new(
        &call,
        ToolActionKind::ReadOnly,
        "label",
        "note.txt",
        "summary",
        evidence.clone(),
    )
    .expect_err("read-only actions do not need proposals");
    assert!(matches!(read_only, ActionProposalError::ReadOnlyAction));

    let blank = ActionProposal::new(
        &call,
        ToolActionKind::WorkspaceWrite,
        " ",
        "note.txt",
        "summary",
        evidence,
    )
    .expect_err("blank label should be rejected");
    assert!(matches!(
        blank,
        ActionProposalError::InvalidText { field: "label", .. }
    ));
}

#[test]
fn action_proposal_evidence_must_match_action_kind() {
    let call = pending_call("run_command");
    let process_intent = ProcessActionIntent::new(
        vec!["cargo".to_owned(), "test".to_owned()],
        Some("crates/merry-runtime".to_owned()),
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("valid process intent");
    let process_proposal = ActionProposal::new(
        &call,
        ToolActionKind::CommandExec,
        "process",
        "cargo test",
        "Run cargo test in the runtime crate",
        ActionProposalEvidence::ProcessAction(process_intent),
    )
    .expect("process evidence matches command exec action");
    assert!(matches!(
        process_proposal.evidence(),
        ActionProposalEvidence::ProcessAction(_)
    ));

    let patch = WorkspacePatchProposal::new(
        "note.txt",
        3,
        5,
        11,
        13,
        "fnv1a64:0123456789abcdef",
        "fnv1a64:fedcba9876543210",
    )
    .expect("valid workspace patch proposal");
    let mismatched = ActionProposal::new(
        &call,
        ToolActionKind::CommandExec,
        "workspace patch",
        "note.txt",
        "Patch evidence cannot stand in for command execution",
        ActionProposalEvidence::WorkspacePatch(patch),
    )
    .expect_err("workspace patch evidence must not match command exec");
    assert!(matches!(
        mismatched,
        ActionProposalError::EvidenceActionKindMismatch {
            action_kind: ToolActionKind::CommandExec
        }
    ));
}
