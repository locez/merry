use crate::{
    PermissionReviewMode, RuntimeTrustLevel,
    memory::MemoryActivationSource,
    model_config::RuntimeModelConfigs,
    runtime::{
        CompactionConfig, Runtime, RuntimeInner,
        tests::support::{
            common::{model_configs_with_primary, runtime_session, session_id},
            model_provider::RecordingModelProvider,
        },
    },
    tool::ToolRegistry,
};
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64},
    },
};

pub(in crate::runtime::tests) fn runtime_with_provider_and_memory_source<S>(
    session: &str,
    provider: RecordingModelProvider,
    source: S,
) -> Runtime
where
    S: MemoryActivationSource + 'static,
{
    let event_buffer_size = NonZeroUsize::new(16).expect("non-zero buffer");
    let id = session_id(session);
    let session_state = runtime_session(id.clone());
    let trajectory = crate::trajectory::RuntimeObservability::new(id.clone(), Vec::new());
    let subagent_event_sink = crate::subagent::SubagentEventSink::new(
        Arc::clone(&session_state),
        None,
        Arc::clone(&trajectory),
        64,
    );
    Runtime {
        inner: Arc::new(RuntimeInner {
            session_id: id.clone(),
            prompt_cache_key: id.clone(),
            session: session_state,
            active_step: Arc::new(AtomicBool::new(false)),
            memory_projection_epoch: AtomicU64::new(0),
            event_buffer_size,
            max_parallel_tool_calls: std::sync::atomic::AtomicUsize::new(4),
            model_configs: model_configs_with_primary(provider),
            role_model_overrides: tokio::sync::RwLock::new(std::collections::BTreeMap::new()),
            automatic_compaction: tokio::sync::RwLock::new(CompactionConfig::default()),
            history_retention: crate::HistoryRetention::default(),
            context_window_tokens: tokio::sync::RwLock::new(None),
            capabilities: crate::RuntimeCapabilities::default(),
            prompt_profile: crate::PromptProfile::default(),
            tool_registry: ToolRegistry::default(),
            tool_admission: None,
            memory_activation_source: Arc::new(source),
            allow_low_risk_apply_patches: false,
            low_risk_process_runner: None,
            read_only_shell_process_runner: None,
            accepted_local_workspace_process_runner: None,
            progress_commentary: false,
            runtime_trust_level: RuntimeTrustLevel::Agent,
            permission_review_mode: tokio::sync::RwLock::new(PermissionReviewMode::DefaultForTrust),
            permission_review_rules: crate::PermissionReviewRules::default(),
            permission_review_workspace_root: None,
            permission_actor: crate::PermissionActor::MainAgent,
            recorded_permission_decisions: tokio::sync::RwLock::new(
                std::collections::VecDeque::new(),
            ),
            permission_admission_source: tokio::sync::RwLock::new(None),
            permissioned_process_runner_factory: None,
            background_process_control: None,
            subagent_manager: None,
            coordinator_plan_tools: false,
            session_store: None,
            tool_batch_active: AtomicBool::new(false),
            activity_hub: Arc::new(crate::SubagentActivityHub::new()),
            subagent_event_sink,
            trajectory,
            durable_memory: None,
            memory_maintenance: false,
            memory_context: None,
        }),
    }
}

pub(in crate::runtime::tests) fn runtime_with_provider(
    session: &str,
    provider: RecordingModelProvider,
) -> Runtime {
    let event_buffer_size = NonZeroUsize::new(16).expect("non-zero buffer");
    let id = session_id(session);
    let session_state = runtime_session(id.clone());
    let trajectory = crate::trajectory::RuntimeObservability::new(id.clone(), Vec::new());
    let subagent_event_sink = crate::subagent::SubagentEventSink::new(
        Arc::clone(&session_state),
        None,
        Arc::clone(&trajectory),
        64,
    );
    Runtime {
        inner: Arc::new(RuntimeInner {
            session_id: id.clone(),
            prompt_cache_key: id.clone(),
            session: session_state,
            active_step: Arc::new(AtomicBool::new(false)),
            memory_projection_epoch: AtomicU64::new(0),
            event_buffer_size,
            max_parallel_tool_calls: std::sync::atomic::AtomicUsize::new(4),
            model_configs: model_configs_with_primary(provider),
            role_model_overrides: tokio::sync::RwLock::new(std::collections::BTreeMap::new()),
            automatic_compaction: tokio::sync::RwLock::new(CompactionConfig::default()),
            history_retention: crate::HistoryRetention::default(),
            context_window_tokens: tokio::sync::RwLock::new(None),
            capabilities: crate::RuntimeCapabilities::default(),
            prompt_profile: crate::PromptProfile::default(),
            tool_registry: ToolRegistry::default(),
            tool_admission: None,
            memory_activation_source: Arc::new(crate::memory::StoredMemoryActivationSource),
            allow_low_risk_apply_patches: false,
            low_risk_process_runner: None,
            read_only_shell_process_runner: None,
            accepted_local_workspace_process_runner: None,
            progress_commentary: false,
            runtime_trust_level: RuntimeTrustLevel::Agent,
            permission_review_mode: tokio::sync::RwLock::new(PermissionReviewMode::DefaultForTrust),
            permission_review_rules: crate::PermissionReviewRules::default(),
            permission_review_workspace_root: None,
            permission_actor: crate::PermissionActor::MainAgent,
            recorded_permission_decisions: tokio::sync::RwLock::new(
                std::collections::VecDeque::new(),
            ),
            permission_admission_source: tokio::sync::RwLock::new(None),
            permissioned_process_runner_factory: None,
            background_process_control: None,
            subagent_manager: None,
            coordinator_plan_tools: false,
            session_store: None,
            tool_batch_active: AtomicBool::new(false),
            activity_hub: Arc::new(crate::SubagentActivityHub::new()),
            subagent_event_sink,
            trajectory,
            durable_memory: None,
            memory_maintenance: false,
            memory_context: None,
        }),
    }
}

pub(in crate::runtime::tests) fn runtime_without_provider_with_memory_source<S>(
    session: &str,
    source: S,
) -> Runtime
where
    S: MemoryActivationSource + 'static,
{
    let event_buffer_size = NonZeroUsize::new(16).expect("non-zero buffer");
    let id = session_id(session);
    let session_state = runtime_session(id.clone());
    let trajectory = crate::trajectory::RuntimeObservability::new(id.clone(), Vec::new());
    let subagent_event_sink = crate::subagent::SubagentEventSink::new(
        Arc::clone(&session_state),
        None,
        Arc::clone(&trajectory),
        64,
    );
    Runtime {
        inner: Arc::new(RuntimeInner {
            session_id: id.clone(),
            prompt_cache_key: id.clone(),
            session: session_state,
            active_step: Arc::new(AtomicBool::new(false)),
            memory_projection_epoch: AtomicU64::new(0),
            event_buffer_size,
            max_parallel_tool_calls: std::sync::atomic::AtomicUsize::new(4),
            model_configs: RuntimeModelConfigs::default(),
            role_model_overrides: tokio::sync::RwLock::new(std::collections::BTreeMap::new()),
            automatic_compaction: tokio::sync::RwLock::new(CompactionConfig::default()),
            history_retention: crate::HistoryRetention::default(),
            context_window_tokens: tokio::sync::RwLock::new(None),
            capabilities: crate::RuntimeCapabilities::default(),
            prompt_profile: crate::PromptProfile::default(),
            tool_registry: ToolRegistry::default(),
            tool_admission: None,
            memory_activation_source: Arc::new(source),
            allow_low_risk_apply_patches: false,
            low_risk_process_runner: None,
            read_only_shell_process_runner: None,
            accepted_local_workspace_process_runner: None,
            progress_commentary: false,
            runtime_trust_level: RuntimeTrustLevel::Agent,
            permission_review_mode: tokio::sync::RwLock::new(PermissionReviewMode::DefaultForTrust),
            permission_review_rules: crate::PermissionReviewRules::default(),
            permission_review_workspace_root: None,
            permission_actor: crate::PermissionActor::MainAgent,
            recorded_permission_decisions: tokio::sync::RwLock::new(
                std::collections::VecDeque::new(),
            ),
            permission_admission_source: tokio::sync::RwLock::new(None),
            permissioned_process_runner_factory: None,
            background_process_control: None,
            subagent_manager: None,
            coordinator_plan_tools: false,
            session_store: None,
            tool_batch_active: AtomicBool::new(false),
            activity_hub: Arc::new(crate::SubagentActivityHub::new()),
            subagent_event_sink,
            trajectory,
            durable_memory: None,
            memory_maintenance: false,
            memory_context: None,
        }),
    }
}
