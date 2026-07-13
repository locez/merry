    #[derive(Clone)]
    struct SuccessfulToolExecutor {
        calls: Arc<AtomicUsize>,
    }
    impl SuccessfulToolExecutor {
        fn new() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ToolExecutor for SuccessfulToolExecutor {
        fn execute<'a>(
            &'a self,
            _call: PendingToolCall,
            _context: ToolExecutionContext,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(ToolExecutionOutcome::succeeded_text("ok\n"))
            })
        }
    }

    #[derive(Clone)]
    struct CancelDuringRuntimeControlExecutor {
        calls: Arc<AtomicUsize>,
        token_seen: Arc<StdMutex<Option<CancellationToken>>>,
    }

    impl CancelDuringRuntimeControlExecutor {
        fn new() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                token_seen: Arc::new(StdMutex::new(None)),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn token_seen(&self) -> CancellationToken {
            self.token_seen
                .lock()
                .expect("token mutex is not poisoned")
                .clone()
                .expect("executor should capture token")
        }
    }

    impl ToolExecutor for CancelDuringRuntimeControlExecutor {
        fn execute<'a>(
            &'a self,
            _call: PendingToolCall,
            context: ToolExecutionContext,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                *self.token_seen.lock().expect("token mutex is not poisoned") =
                    Some(context.cancellation_token().clone());
                context.cancellation_token().cancel();
                Ok(ToolExecutionOutcome::succeeded_text(
                    "control state committed\n",
                ))
            })
        }
    }

    #[derive(Clone)]
    struct ProposingToolExecutor {
        execute_calls: Arc<AtomicUsize>,
        propose_calls: Arc<AtomicUsize>,
        wait_for_cancel: bool,
        record_approved_proposal: Arc<StdMutex<Vec<bool>>>,
        attach_execution_evidence: bool,
        preflight_outcome: Option<ToolExecutionOutcome>,
        propose_started: Option<Arc<Notify>>,
        release_propose: Option<Arc<Notify>>,
        execute_started: Option<Arc<Notify>>,
        release_execute: Option<Arc<Notify>>,
    }

    impl ProposingToolExecutor {
        fn immediate() -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                wait_for_cancel: false,
                record_approved_proposal: Arc::new(StdMutex::new(Vec::new())),
                attach_execution_evidence: true,
                preflight_outcome: None,
                propose_started: None,
                release_propose: None,
                execute_started: None,
                release_execute: None,
            }
        }

        fn blocking_proposal() -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                wait_for_cancel: false,
                record_approved_proposal: Arc::new(StdMutex::new(Vec::new())),
                attach_execution_evidence: true,
                preflight_outcome: None,
                propose_started: Some(Arc::new(Notify::new())),
                release_propose: Some(Arc::new(Notify::new())),
                execute_started: None,
                release_execute: None,
            }
        }

        fn blocking() -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                wait_for_cancel: false,
                record_approved_proposal: Arc::new(StdMutex::new(Vec::new())),
                attach_execution_evidence: true,
                preflight_outcome: None,
                propose_started: None,
                release_propose: None,
                execute_started: Some(Arc::new(Notify::new())),
                release_execute: Some(Arc::new(Notify::new())),
            }
        }

        fn cancelling() -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                wait_for_cancel: true,
                record_approved_proposal: Arc::new(StdMutex::new(Vec::new())),
                attach_execution_evidence: true,
                preflight_outcome: None,
                propose_started: None,
                release_propose: None,
                execute_started: None,
                release_execute: None,
            }
        }

        fn missing_execution_evidence() -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                wait_for_cancel: false,
                record_approved_proposal: Arc::new(StdMutex::new(Vec::new())),
                attach_execution_evidence: false,
                preflight_outcome: None,
                propose_started: None,
                release_propose: None,
                execute_started: None,
                release_execute: None,
            }
        }

        fn with_preflight_outcome(outcome: ToolExecutionOutcome) -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                wait_for_cancel: false,
                record_approved_proposal: Arc::new(StdMutex::new(Vec::new())),
                attach_execution_evidence: true,
                preflight_outcome: Some(outcome),
                propose_started: None,
                release_propose: None,
                execute_started: None,
                release_execute: None,
            }
        }

        fn execute_count(&self) -> usize {
            self.execute_calls.load(Ordering::SeqCst)
        }

        fn propose_count(&self) -> usize {
            self.propose_calls.load(Ordering::SeqCst)
        }

        fn approved_proposal_seen(&self) -> Vec<bool> {
            self.record_approved_proposal
                .lock()
                .expect("approved proposal records mutex should not be poisoned")
                .clone()
        }

        async fn wait_for_propose_start(&self) {
            self.propose_started
                .as_ref()
                .expect("blocking proposer has a start notification")
                .notified()
                .await;
        }

        fn release_propose(&self) {
            self.release_propose
                .as_ref()
                .expect("blocking proposer has a release notification")
                .notify_one();
        }

        async fn wait_for_execute_start(&self) {
            self.execute_started
                .as_ref()
                .expect("blocking executor has a start notification")
                .notified()
                .await;
        }

        fn release_execute(&self) {
            self.release_execute
                .as_ref()
                .expect("blocking executor has a release notification")
                .notify_one();
        }
    }

    impl ToolExecutor for ProposingToolExecutor {
        fn propose<'a>(
            &'a self,
            call: PendingToolCall,
            context: ToolExecutionContext,
        ) -> ToolActionProposalFuture<'a> {
            Box::pin(async move {
                self.propose_calls.fetch_add(1, Ordering::SeqCst);
                if self.wait_for_cancel {
                    context.cancellation_token().cancelled().await;
                    return Err(ToolExecutionError::Cancelled);
                }
                if let Some(started) = self.propose_started.as_ref() {
                    started.notify_one();
                    self.release_propose
                        .as_ref()
                        .expect("blocking proposer has a release notification")
                        .notified()
                        .await;
                }
                if let Some(outcome) = self.preflight_outcome.clone() {
                    return Ok(ToolActionPreflight::Outcome(outcome));
                }

                let patch = WorkspacePatchProposal::new(
                    "notes/proposed.txt",
                    3,
                    7,
                    20,
                    24,
                    "fnv1a64:0000000000000001",
                    "fnv1a64:0000000000000002",
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
                    .push(context.approved_workspace_patch().is_some());
                if let Some(started) = self.execute_started.as_ref() {
                    started.notify_one();
                    self.release_execute
                        .as_ref()
                        .expect("blocking executor has a release notification")
                        .notified()
                        .await;
                }
                if !self.attach_execution_evidence {
                    return Ok(ToolExecutionOutcome::succeeded_text(
                        "patched without evidence\n",
                    ));
                }
                let evidence = WorkspacePatchExecutionEvidence::new(
                    "notes/proposed.txt",
                    3,
                    7,
                    20,
                    24,
                    "fnv1a64:0000000000000001",
                    "fnv1a64:0000000000000002",
                )
                .expect("test execution evidence is valid");
                Ok(ToolExecutionOutcome::succeeded_text("patched\n")
                    .with_execution_evidence(ActionExecutionEvidence::WorkspacePatch(evidence)))
            })
        }
    }
