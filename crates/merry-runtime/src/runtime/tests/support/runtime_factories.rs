    fn runtime_with_provider_and_memory_source<S>(
        session: &str,
        provider: RecordingModelProvider,
        source: S,
    ) -> Runtime
    where
        S: MemoryActivationSource + 'static,
    {
        let event_buffer_size = NonZeroUsize::new(16).expect("non-zero buffer");
        let id = session_id(session);
        let (session_state, plan_controller) =
            runtime_session_and_plan_controller(id.clone(), event_buffer_size);
        Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: id,
                session: session_state,
                active_step: Arc::new(AtomicBool::new(false)),
                memory_projection_epoch: AtomicU64::new(0),
                event_buffer_size,
                max_parallel_tool_calls: NonZeroUsize::new(4).expect("non-zero limit"),
                model_configs: model_configs_with_primary(provider),
                primary_model_override: tokio::sync::RwLock::new(None),
                automatic_compaction: tokio::sync::RwLock::new(
                    AutomaticCompactionConfig::default(),
                ),
                context_window_tokens: tokio::sync::RwLock::new(None),
                capabilities: crate::RuntimeCapabilities::default(),
                tool_registry: ToolRegistry::default(),
                memory_activation_source: Arc::new(source),
                allow_low_risk_workspace_patches: false,
                low_risk_process_runner: None,
                read_only_shell_process_runner: None,
                accepted_local_workspace_process_runner: None,
                progress_commentary: false,
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
            }),
        }
    }
    fn runtime_with_provider(session: &str, provider: RecordingModelProvider) -> Runtime {
        let event_buffer_size = NonZeroUsize::new(16).expect("non-zero buffer");
        let id = session_id(session);
        let (session_state, plan_controller) =
            runtime_session_and_plan_controller(id.clone(), event_buffer_size);
        Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: id,
                session: session_state,
                active_step: Arc::new(AtomicBool::new(false)),
                memory_projection_epoch: AtomicU64::new(0),
                event_buffer_size,
                max_parallel_tool_calls: NonZeroUsize::new(4).expect("non-zero limit"),
                model_configs: model_configs_with_primary(provider),
                primary_model_override: tokio::sync::RwLock::new(None),
                automatic_compaction: tokio::sync::RwLock::new(
                    AutomaticCompactionConfig::default(),
                ),
                context_window_tokens: tokio::sync::RwLock::new(None),
                capabilities: crate::RuntimeCapabilities::default(),
                tool_registry: ToolRegistry::default(),
                memory_activation_source: Arc::new(crate::memory::StoredMemoryActivationSource),
                allow_low_risk_workspace_patches: false,
                low_risk_process_runner: None,
                read_only_shell_process_runner: None,
                accepted_local_workspace_process_runner: None,
                progress_commentary: false,
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
            }),
        }
    }

    fn runtime_without_provider_with_memory_source<S>(session: &str, source: S) -> Runtime
    where
        S: MemoryActivationSource + 'static,
    {
        let event_buffer_size = NonZeroUsize::new(16).expect("non-zero buffer");
        let id = session_id(session);
        let (session_state, plan_controller) =
            runtime_session_and_plan_controller(id.clone(), event_buffer_size);
        Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: id,
                session: session_state,
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
                tool_registry: ToolRegistry::default(),
                memory_activation_source: Arc::new(source),
                allow_low_risk_workspace_patches: false,
                low_risk_process_runner: None,
                read_only_shell_process_runner: None,
                accepted_local_workspace_process_runner: None,
                progress_commentary: false,
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
            }),
        }
    }
