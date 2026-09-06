use crate::{
    process::{
        PermissionedProcessRunnerFactory, ProcessActionIntent, ProcessEnvPolicy, ProcessExitStatus,
        ProcessRunner, ProcessRunnerContext, ProcessRunnerError, ProcessRunnerFuture,
        ProcessRunnerOutput,
    },
    tool::{
        ActionExecutionEvidence, ActionProposal, ActionProposalEvidence, ToolActionKind,
        ToolActionPreflight, ToolActionProposalFuture, ToolExecutionContext, ToolExecutionOutcome,
        ToolExecutor, ToolExecutorFuture, WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
    },
};
use merry_core::PendingToolCall;
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Clone)]
pub(in crate::runtime::tests) struct ProcessProposingToolExecutor {
    pub(in crate::runtime::tests) execute_calls: Arc<AtomicUsize>,
    pub(in crate::runtime::tests) propose_calls: Arc<AtomicUsize>,
    pub(in crate::runtime::tests) argv: Vec<String>,
    pub(in crate::runtime::tests) stdin_text: Option<String>,
}

impl ProcessProposingToolExecutor {
    pub(in crate::runtime::tests) fn new() -> Self {
        Self::with_argv(["rustc", "--version"])
    }

    pub(in crate::runtime::tests) fn with_argv<const N: usize>(argv: [&str; N]) -> Self {
        Self {
            execute_calls: Arc::new(AtomicUsize::new(0)),
            propose_calls: Arc::new(AtomicUsize::new(0)),
            argv: argv.into_iter().map(str::to_owned).collect(),
            stdin_text: None,
        }
    }

    pub(in crate::runtime::tests) fn with_stdin_text(stdin_text: impl Into<String>) -> Self {
        Self {
            execute_calls: Arc::new(AtomicUsize::new(0)),
            propose_calls: Arc::new(AtomicUsize::new(0)),
            argv: ["cargo", "test", "-p", "merry-runtime"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            stdin_text: Some(stdin_text.into()),
        }
    }

    pub(in crate::runtime::tests) fn execute_count(&self) -> usize {
        self.execute_calls.load(Ordering::SeqCst)
    }

    pub(in crate::runtime::tests) fn propose_count(&self) -> usize {
        self.propose_calls.load(Ordering::SeqCst)
    }
}

impl ToolExecutor for ProcessProposingToolExecutor {
    fn propose<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolActionProposalFuture<'a> {
        Box::pin(async move {
            self.propose_calls.fetch_add(1, Ordering::SeqCst);
            let intent = ProcessActionIntent::new(
                self.argv.clone(),
                Some(".".to_owned()),
                ProcessEnvPolicy::empty(),
                self.stdin_text.clone(),
                16 * 1024,
                16 * 1024,
            )
            .expect("test process intent is valid");
            Ok(ToolActionPreflight::Proposal(
                ActionProposal::new(
                    &call,
                    ToolActionKind::CommandExec,
                    "process action",
                    self.argv.join(" "),
                    "Run proposed process action.",
                    ActionProposalEvidence::ProcessAction(intent),
                )
                .expect("test process action proposal is valid"),
            ))
        })
    }

    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.execute_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolExecutionOutcome::succeeded_text(
                "process execution must not be reached in SP1\n",
            ))
        })
    }
}

#[derive(Clone)]
pub(in crate::runtime::tests) struct FakeProcessRunner {
    pub(in crate::runtime::tests) calls: Arc<AtomicUsize>,
    pub(in crate::runtime::tests) observed_intents: Arc<StdMutex<Vec<ProcessActionIntent>>>,
    pub(in crate::runtime::tests) response: Arc<StdMutex<Option<FakeProcessRunnerResponse>>>,
}

impl FakeProcessRunner {
    pub(in crate::runtime::tests) fn succeeding() -> Self {
        Self::with_response(FakeProcessRunnerResponse::Success {
            stdout_text: "runtime tests passed\n".to_owned(),
            stdout_truncated: false,
            stderr_text: String::new(),
            stderr_truncated: false,
        })
    }

    pub(in crate::runtime::tests) fn succeeding_with_truncated_stdout() -> Self {
        Self::with_response(FakeProcessRunnerResponse::Success {
            stdout_text: "partial runtime output\n".to_owned(),
            stdout_truncated: true,
            stderr_text: String::new(),
            stderr_truncated: false,
        })
    }

    pub(in crate::runtime::tests) fn succeeding_with_non_utf8_output() -> Self {
        Self::with_response(FakeProcessRunnerResponse::SuccessBytes {
            stdout_data: vec![0xff, 0x00, b'a'],
            stdout_truncated: false,
            stderr_data: vec![b'o', 0xfe],
            stderr_truncated: false,
        })
    }

    pub(in crate::runtime::tests) fn failing() -> Self {
        Self::with_response(FakeProcessRunnerResponse::Failure {
            stdout_text: String::new(),
            stdout_truncated: false,
            stderr_text: "permission denied\n".to_owned(),
            stderr_truncated: false,
        })
    }

    pub(in crate::runtime::tests) fn cancelling() -> Self {
        Self::with_response(FakeProcessRunnerResponse::Error(
            ProcessRunnerError::Cancelled,
        ))
    }

    pub(in crate::runtime::tests) fn infrastructure_failure(message: &str) -> Self {
        Self::with_response(FakeProcessRunnerResponse::Error(
            ProcessRunnerError::infrastructure(message),
        ))
    }

    pub(in crate::runtime::tests) fn succeeding_then_cancelling_token() -> Self {
        Self::with_response(FakeProcessRunnerResponse::SuccessThenCancel {
            stdout_text: "runtime tests passed after token cancellation\n".to_owned(),
            stdout_truncated: false,
            stderr_text: String::new(),
            stderr_truncated: false,
        })
    }

    pub(in crate::runtime::tests) fn with_response(response: FakeProcessRunnerResponse) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            observed_intents: Arc::new(StdMutex::new(Vec::new())),
            response: Arc::new(StdMutex::new(Some(response))),
        }
    }

    pub(in crate::runtime::tests) fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub(in crate::runtime::tests) fn observed_intents(&self) -> Vec<ProcessActionIntent> {
        self.observed_intents
            .lock()
            .expect("observed intents mutex should not be poisoned")
            .clone()
    }
}

pub(in crate::runtime::tests) enum FakeProcessRunnerResponse {
    Success {
        stdout_text: String,
        stdout_truncated: bool,
        stderr_text: String,
        stderr_truncated: bool,
    },
    SuccessThenCancel {
        stdout_text: String,
        stdout_truncated: bool,
        stderr_text: String,
        stderr_truncated: bool,
    },
    SuccessBytes {
        stdout_data: Vec<u8>,
        stdout_truncated: bool,
        stderr_data: Vec<u8>,
        stderr_truncated: bool,
    },
    Failure {
        stdout_text: String,
        stdout_truncated: bool,
        stderr_text: String,
        stderr_truncated: bool,
    },
    Error(ProcessRunnerError),
}

impl ProcessRunner for FakeProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.observed_intents
                .lock()
                .expect("observed intents mutex should not be poisoned")
                .push(intent.clone());
            if context.cancellation_token().is_cancelled() {
                return Err(ProcessRunnerError::Cancelled);
            }

            let response = self
                .response
                .lock()
                .expect("process response mutex should not be poisoned")
                .take()
                .expect("scripted process response should exist");
            match response {
                FakeProcessRunnerResponse::Success {
                    stdout_text,
                    stdout_truncated,
                    stderr_text,
                    stderr_truncated,
                } => ProcessRunnerOutput::new(
                    &intent,
                    ProcessExitStatus::Exited(0),
                    stdout_text,
                    stdout_truncated,
                    stderr_text,
                    stderr_truncated,
                )
                .map_err(|source| ProcessRunnerError::infrastructure(source.to_string())),
                FakeProcessRunnerResponse::SuccessThenCancel {
                    stdout_text,
                    stdout_truncated,
                    stderr_text,
                    stderr_truncated,
                } => {
                    let output = ProcessRunnerOutput::new(
                        &intent,
                        ProcessExitStatus::Exited(0),
                        stdout_text,
                        stdout_truncated,
                        stderr_text,
                        stderr_truncated,
                    )
                    .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))?;
                    context.cancellation_token().cancel();
                    Ok(output)
                }
                FakeProcessRunnerResponse::SuccessBytes {
                    stdout_data,
                    stdout_truncated,
                    stderr_data,
                    stderr_truncated,
                } => ProcessRunnerOutput::from_bytes(
                    &intent,
                    ProcessExitStatus::Exited(0),
                    stdout_data,
                    stdout_truncated,
                    stderr_data,
                    stderr_truncated,
                )
                .map_err(|source| ProcessRunnerError::infrastructure(source.to_string())),
                FakeProcessRunnerResponse::Failure {
                    stdout_text,
                    stdout_truncated,
                    stderr_text,
                    stderr_truncated,
                } => ProcessRunnerOutput::new(
                    &intent,
                    ProcessExitStatus::Exited(1),
                    stdout_text,
                    stdout_truncated,
                    stderr_text,
                    stderr_truncated,
                )
                .map_err(|source| ProcessRunnerError::infrastructure(source.to_string())),
                FakeProcessRunnerResponse::Error(error) => Err(error),
            }
        })
    }
}

#[derive(Clone)]
pub(in crate::runtime::tests) struct RecordingPermissionedProcessRunnerFactory {
    pub(in crate::runtime::tests) runner: Arc<dyn ProcessRunner>,
    pub(in crate::runtime::tests) calls: Arc<AtomicUsize>,
    pub(in crate::runtime::tests) observed_network_requests: Arc<StdMutex<Vec<bool>>>,
    pub(in crate::runtime::tests) capabilities_satisfied: Arc<AtomicBool>,
}

impl RecordingPermissionedProcessRunnerFactory {
    pub(in crate::runtime::tests) fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self {
            runner,
            calls: Arc::new(AtomicUsize::new(0)),
            observed_network_requests: Arc::new(StdMutex::new(Vec::new())),
            capabilities_satisfied: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(in crate::runtime::tests) fn with_capabilities_satisfied(self) -> Self {
        self.capabilities_satisfied.store(true, Ordering::SeqCst);
        self
    }

    pub(in crate::runtime::tests) fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub(in crate::runtime::tests) fn observed_network_requests(&self) -> Vec<bool> {
        self.observed_network_requests
            .lock()
            .expect("observed permission requests mutex should not be poisoned")
            .clone()
    }
}

impl PermissionedProcessRunnerFactory for RecordingPermissionedProcessRunnerFactory {
    fn request_capabilities_are_satisfied(
        &self,
        _request: &crate::PermissionRequest,
    ) -> Result<bool, ProcessRunnerError> {
        Ok(self.capabilities_satisfied.load(Ordering::SeqCst))
    }

    fn runner_for(&self, request: &crate::PermissionRequest) -> Arc<dyn ProcessRunner> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.observed_network_requests
            .lock()
            .expect("observed permission requests mutex should not be poisoned")
            .push(request.requests_network());
        Arc::clone(&self.runner)
    }
}

#[derive(Clone)]
pub(in crate::runtime::tests) struct StaticPermissionAdmissionSource {
    pub(in crate::runtime::tests) decision: crate::PermissionAdmissionDecision,
    pub(in crate::runtime::tests) calls: Arc<AtomicUsize>,
}

impl StaticPermissionAdmissionSource {
    pub(in crate::runtime::tests) fn approving() -> Self {
        Self {
            decision: crate::PermissionAdmissionDecision::approved("host approved"),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(in crate::runtime::tests) fn denying() -> Self {
        Self {
            decision: crate::PermissionAdmissionDecision::denied("host denied"),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(in crate::runtime::tests) fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl crate::PermissionAdmissionSource for StaticPermissionAdmissionSource {
    fn review<'a>(
        &'a self,
        _request: crate::PermissionRequest,
        _context: crate::PermissionAdmissionContext,
    ) -> crate::PermissionAdmissionFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.decision.clone())
        })
    }
}

#[derive(Clone)]
pub(in crate::runtime::tests) struct CancellingOptInPatchExecutor {
    pub(in crate::runtime::tests) execute_calls: Arc<AtomicUsize>,
    pub(in crate::runtime::tests) propose_calls: Arc<AtomicUsize>,
    pub(in crate::runtime::tests) side_effect: Arc<AtomicBool>,
    pub(in crate::runtime::tests) record_approved_proposal: Arc<StdMutex<Vec<bool>>>,
}

impl CancellingOptInPatchExecutor {
    pub(in crate::runtime::tests) fn new() -> Self {
        Self {
            execute_calls: Arc::new(AtomicUsize::new(0)),
            propose_calls: Arc::new(AtomicUsize::new(0)),
            side_effect: Arc::new(AtomicBool::new(false)),
            record_approved_proposal: Arc::new(StdMutex::new(Vec::new())),
        }
    }

    pub(in crate::runtime::tests) fn execute_count(&self) -> usize {
        self.execute_calls.load(Ordering::SeqCst)
    }

    pub(in crate::runtime::tests) fn propose_count(&self) -> usize {
        self.propose_calls.load(Ordering::SeqCst)
    }

    pub(in crate::runtime::tests) fn side_effect_happened(&self) -> bool {
        self.side_effect.load(Ordering::SeqCst)
    }

    pub(in crate::runtime::tests) fn approved_proposal_seen(&self) -> Vec<bool> {
        self.record_approved_proposal
            .lock()
            .expect("approved proposal records mutex should not be poisoned")
            .clone()
    }
}

impl ToolExecutor for CancellingOptInPatchExecutor {
    fn propose<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolActionProposalFuture<'a> {
        Box::pin(async move {
            self.propose_calls.fetch_add(1, Ordering::SeqCst);
            let patch = WorkspacePatchProposal::new(
                "notes/proposed.txt",
                3,
                7,
                20,
                24,
                "fnv1a64:0000000000000003",
                "fnv1a64:0000000000000004",
            )
            .expect("test proposal metadata is valid");
            Ok(ToolActionPreflight::Proposal(
                ActionProposal::new(
                    &call,
                    ToolActionKind::WorkspaceWrite,
                    "workspace patch",
                    "notes/proposed.txt",
                    "Replace one matched preimage in notes/proposed.txt",
                    ActionProposalEvidence::WorkspacePatch(patch),
                )
                .expect("test action proposal is valid"),
            ))
        })
    }

    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.execute_calls.fetch_add(1, Ordering::SeqCst);
            self.record_approved_proposal
                .lock()
                .expect("approved proposal records mutex should not be poisoned")
                .push(context.approved_apply_patch().is_some());
            self.side_effect.store(true, Ordering::SeqCst);
            context.cancellation_token().cancel();
            let evidence = WorkspacePatchExecutionEvidence::new(
                "notes/proposed.txt",
                3,
                7,
                20,
                24,
                "fnv1a64:0000000000000003",
                "fnv1a64:0000000000000004",
            )
            .expect("test execution evidence is valid");
            Ok(ToolExecutionOutcome::succeeded_text("patched\n")
                .with_execution_evidence(ActionExecutionEvidence::WorkspacePatch(evidence)))
        })
    }
}
