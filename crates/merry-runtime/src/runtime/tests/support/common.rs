    fn trace_output_buffer() -> &'static Arc<StdMutex<Vec<u8>>> {
        #[derive(Clone)]
        struct Buffer(Arc<StdMutex<Vec<u8>>>);

        impl std::io::Write for Buffer {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .expect("buffer mutex should not be poisoned")
                    .extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        static TRACE_OUTPUT: OnceLock<Arc<StdMutex<Vec<u8>>>> = OnceLock::new();
        TRACE_OUTPUT.get_or_init(|| {
            use tracing_subscriber::{fmt, prelude::*};

            let bytes = Arc::new(StdMutex::new(Vec::new()));
            let writer_bytes = Arc::clone(&bytes);
            let subscriber = tracing_subscriber::registry().with(
                fmt::layer()
                    .json()
                    .with_writer(move || Buffer(Arc::clone(&writer_bytes))),
            );
            tracing::subscriber::set_global_default(subscriber)
                .expect("test tracing subscriber should install once");
            bytes
        })
    }
    async fn capture_traces_for<F, R>(trace_marker: &str, future: F) -> (R, String)
    where
        F: Future<Output = R>,
    {
        let bytes = Arc::clone(trace_output_buffer());
        let start = bytes
            .lock()
            .expect("buffer mutex should not be poisoned")
            .len();
        let result = future.await;
        let text = {
            let guard = bytes.lock().expect("buffer mutex should not be poisoned");
            String::from_utf8(guard[start..].to_vec()).expect("trace output should be UTF-8")
        };
        let text = text
            .lines()
            .filter(|line| line.contains(trace_marker))
            .collect::<Vec<_>>()
            .join("\n");
        (result, text)
    }

    fn model_configs_with_primary(provider: RecordingModelProvider) -> RuntimeModelConfigs {
        let mut configs = RuntimeModelConfigs::default();
        configs.insert(
            RuntimeModelRole::Primary,
            Arc::new(provider),
            model_name(),
            ModelRetryPolicy::default(),
        );
        configs
    }

    fn runtime_session_and_plan_controller(
        session_id: SessionId,
        event_buffer_size: NonZeroUsize,
    ) -> (Arc<Mutex<SessionState>>, PlanController) {
        let session = Arc::new(Mutex::new(SessionState::new(session_id)));
        let (plan_controller, _events) =
            PlanController::start(Arc::clone(&session), None, event_buffer_size);
        (session, plan_controller)
    }

    fn runtime_inner() -> RuntimeInner {
        let session_id = SessionId::new("runtime-send-test").expect("valid session id");
        let event_buffer_size = NonZeroUsize::new(1).expect("non-zero buffer");
        let (session, plan_controller) =
            runtime_session_and_plan_controller(session_id.clone(), event_buffer_size);
        RuntimeInner {
            session_id: session_id.clone(),
            session,
            active_step: Arc::new(AtomicBool::new(false)),
            memory_projection_epoch: AtomicU64::new(0),
            event_buffer_size,
            max_parallel_tool_calls: NonZeroUsize::new(4).expect("non-zero limit"),
            model_configs: RuntimeModelConfigs::default(),
            primary_model_override: tokio::sync::RwLock::new(None),
            automatic_compaction: tokio::sync::RwLock::new(
                AutomaticCompactionConfig::default(),
            ),
            context_window_tokens: tokio::sync::RwLock::new(None),
            capabilities: crate::RuntimeCapabilities::default(),
            prompt_profile: crate::PromptProfile::default(),
            progress_commentary: false,
            tool_registry: ToolRegistry::default(),
            tool_admission: None,
            memory_activation_source: Arc::new(crate::memory::StoredMemoryActivationSource),
            allow_low_risk_workspace_patches: false,
            low_risk_process_runner: None,
            read_only_shell_process_runner: None,
            accepted_local_workspace_process_runner: None,
            runtime_trust_level: RuntimeTrustLevel::Agent,
            permission_review_mode: PermissionReviewMode::DefaultForTrust,
            permission_admission_source: None,
            permissioned_process_runner_factory: None,
            subagent_manager: None,
            coordinator_plan_tools: false,
            plan_controller,
            plan_subagent_control: None,
            plan_subagent_scope: None,
            session_store: None,
            tool_batch_active: AtomicBool::new(false),
            activity_hub: Arc::new(crate::SubagentActivityHub::new()),
        }
    }

    fn artifact_id(value: &str) -> ArtifactId {
        ArtifactId::new(value).expect("valid artifact id")
    }

    fn session_id(value: &str) -> SessionId {
        SessionId::new(value).expect("valid session id")
    }

    fn model_name() -> ModelName {
        ModelName::new("fake/model").expect("valid model name")
    }

    fn named_model(value: &str) -> ModelName {
        ModelName::new(value).expect("valid model name")
    }

    fn accepted_local_workspace_process_admission() -> AcceptedLocalWorkspaceProcessAdmission {
        AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace()
    }

    fn completed_event() -> ModelEvent {
        ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("model result")],
                FinishReason::Stop,
                None,
            ),
        }
    }

    fn completed_event_with(outputs: Vec<ModelOutput>, finish_reason: FinishReason) -> ModelEvent {
        ModelEvent::Completed {
            response: ModelResponse::new(outputs, finish_reason, None),
        }
    }

    fn permission_review_completed_event(decision: &str, rationale: &str) -> ModelEvent {
        let output = format!(
            r#"{{"schema_version":"permission_review.v1","decision":"{decision}","risk":"low","user_authorization":"high","rationale":"{rationale}"}}"#
        );
        completed_event_with(vec![ModelOutput::text(&output)], FinishReason::Stop)
    }

    fn event_kind_names(events: &[RuntimeJournalEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|event| match event.payload {
                RuntimeJournalPayload::SessionStarted => "SessionStarted",
                RuntimeJournalPayload::StepStarted => "StepStarted",
                RuntimeJournalPayload::ModelRetryAttemptStarted { .. } => "ModelRetryAttemptStarted",
                RuntimeJournalPayload::ModelRetryScheduled { .. } => "ModelRetryScheduled",
                RuntimeJournalPayload::ModelRetryExhausted { .. } => "ModelRetryExhausted",
                RuntimeJournalPayload::CompactionStarted => "CompactionStarted",
                RuntimeJournalPayload::CompactionCompleted { .. } => "CompactionCompleted",
                RuntimeJournalPayload::SessionUsageUpdated { .. } => "SessionUsageUpdated",
                RuntimeJournalPayload::StepCompleted => "StepCompleted",
                RuntimeJournalPayload::Cancelled { .. } => "Cancelled",
                RuntimeJournalPayload::Failed { .. } => "Failed",
                RuntimeJournalPayload::ArtifactRecorded { .. } => "ArtifactRecorded",
                RuntimeJournalPayload::AssistantOutputDelta { .. } => "AssistantOutputDelta",
                RuntimeJournalPayload::AssistantOutputRecorded { .. } => "AssistantOutputRecorded",
                RuntimeJournalPayload::EvidenceReferenced { .. } => "EvidenceReferenced",
                RuntimeJournalPayload::ToolCallPending { .. } => "ToolCallPending",
                RuntimeJournalPayload::BridgeToolCallRequested { .. } => "BridgeToolCallRequested",
                RuntimeJournalPayload::ToolCallResolved { .. } => "ToolCallResolved",
                RuntimeJournalPayload::SkillUsed { .. } => "SkillUsed",
                _ => "Unknown",
            })
            .collect()
    }

    fn failed_code(events: &[RuntimeJournalEvent]) -> Option<&str> {
        events.iter().find_map(|event| match &event.payload {
            RuntimeJournalPayload::Failed { diagnostic } => Some(diagnostic.code()),
            _ => None,
        })
    }

    async fn collect_step(
        runtime: &Runtime,
        text: &str,
        context: crate::StepContext,
    ) -> Vec<RuntimeJournalEvent> {
        runtime
            .step(
                crate::StepInput::user_text(text).expect("valid step input"),
                context,
            )
            .expect("step should start")
            .collect()
            .await
    }

    trait RuntimeSessionStateTestExt {
        fn record_test_user_message_body(&mut self, text: &str) -> Result<(), RuntimeError>;
        fn record_test_tool_call_pending(
            &mut self,
            call: PendingToolCall,
        ) -> Result<RuntimeJournalEvent, ErrorInfo>;
    }

    impl RuntimeSessionStateTestExt for SessionState {
        fn record_test_user_message_body(&mut self, text: &str) -> Result<(), RuntimeError> {
            let turn_id = self.begin_model_turn()?;
            self.record_user_message_body(turn_id, text)?;
            self.close_model_response(turn_id, false)
        }

        fn record_test_tool_call_pending(
            &mut self,
            call: PendingToolCall,
        ) -> Result<RuntimeJournalEvent, ErrorInfo> {
            let turn_id = self.begin_model_turn().map_err(runtime_test_turn_diagnostic)?;
            let event = self.record_tool_call_pending(turn_id, call)?;
            self.close_model_response(turn_id, true)
                .map_err(runtime_test_turn_diagnostic)?;
            Ok(event)
        }
    }

    fn runtime_test_turn_diagnostic(error: RuntimeError) -> ErrorInfo {
        ErrorInfo::new("test_model_turn", &error.to_string())
            .expect("test model turn diagnostic should be valid")
    }

    fn pending_tool_call(id: &str) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new(id).expect("valid tool call id"),
            ToolName::new("lookup").expect("valid tool name"),
            ToolCallArguments::new(Default::default()),
        )
    }

    fn model_tool_call(id: &str) -> ModelToolCall {
        ModelToolCall::new(
            ModelToolCallId::new(id).expect("valid model tool call id"),
            ToolName::new("lookup").expect("valid tool name"),
            ToolArguments::new(Default::default()),
        )
    }
