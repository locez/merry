use super::config::{CompactionConfig, HistoryRetention};
use crate::{
    AcceptedLocalWorkspaceProcessAdmission, FileSessionStore, ProcessRunner, RuntimeCapabilities,
    RuntimeModelRole,
    memory::MemoryActivationSource,
    model_config::{ModelProviderConfig, RuntimeModelConfigs},
    permission::{
        MAX_RECORDED_PERMISSION_DECISIONS, PermissionActor, PermissionAdmissionDecision,
        PermissionAdmissionSource, PermissionRequest, PermissionReviewMode, PermissionReviewRules,
        RecordedPermissionDecision, RuntimeTrustLevel,
    },
    process::{BackgroundProcessControl, PermissionedProcessRunnerFactory},
    session::SessionState,
    subagent::{SubagentActivityHub, SubagentEventSink, SubagentManager},
    tool::ToolRegistry,
    trajectory::RuntimeObservability,
};
use merry_core::SessionId;
use std::{
    collections::{BTreeMap, VecDeque},
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex, RwLock};

/// Session durability, locking, and history retention settings owned by the runtime.
pub(super) struct SessionPersistenceState {
    pub(super) store: Option<FileSessionStore>,
    pub(super) reservation: std::sync::Mutex<Option<crate::SessionReservation>>,
    pub(super) history_retention: HistoryRetention,
}

/// Model provider routing, prompt composition, and context-compaction controls.
pub(super) struct ModelContextState {
    pub(super) prompt_cache_key: SessionId,
    pub(super) configs: RuntimeModelConfigs,
    pub(super) role_overrides: RwLock<BTreeMap<RuntimeModelRole, Option<ModelProviderConfig>>>,
    pub(super) automatic_compaction: RwLock<CompactionConfig>,
    pub(super) context_window_tokens: RwLock<Option<NonZeroU64>>,
    pub(super) prompt_profile: crate::PromptProfile,
    pub(super) progress_commentary: bool,
}

impl ModelContextState {
    pub(super) async fn config(&self, role: RuntimeModelRole) -> Option<ModelProviderConfig> {
        if let Some(override_entry) = self.role_overrides.read().await.get(&role) {
            return override_entry.clone();
        }
        self.configs.get(role)
    }

    pub(super) async fn config_with_primary_fallback(
        &self,
        role: RuntimeModelRole,
    ) -> Option<ModelProviderConfig> {
        if role != RuntimeModelRole::Primary
            && let Some(config) = self.config(role).await
        {
            return Some(config);
        }
        self.config(RuntimeModelRole::Primary).await
    }

    pub(super) fn has_primary(&self) -> bool {
        self.configs.contains_role(RuntimeModelRole::Primary)
    }
}

/// Ephemeral memory activation and durable memory maintenance state.
pub(super) struct MemoryState {
    pub(super) activation_source: Arc<dyn MemoryActivationSource>,
    pub(super) projection_epoch: AtomicU64,
    pub(super) durable: Option<Arc<crate::DurableMemory>>,
    pub(super) maintenance_enabled: bool,
    pub(super) frozen_context: Option<Arc<str>>,
}

/// Registered tool surface, admission, concurrency, and hook routing state.
pub(super) struct ToolDispatchState {
    pub(super) registry: ToolRegistry,
    pub(super) admission: Option<crate::ToolAdmission>,
    pub(super) max_parallel_calls: AtomicUsize,
    pub(super) batch_active: AtomicBool,
    pub(super) allow_low_risk_workspace_patches: bool,
    pub(super) hook_handler: Option<Arc<dyn crate::RuntimeHookHandler>>,
    pub(super) bridge_hook_subscriptions: crate::HookSubscriptions,
}

impl ToolDispatchState {
    pub(super) fn visible_specs(&self) -> Vec<merry_core::ToolSpec> {
        self.registry.tool_specs()
    }

    pub(super) fn begin_batch(&self) -> ToolBatchScope<'_> {
        debug_assert!(!self.batch_active.swap(true, Ordering::AcqRel));
        ToolBatchScope { tools: self }
    }

    pub(super) fn is_batch_active(&self) -> bool {
        self.batch_active.load(Ordering::Acquire)
    }

    pub(super) fn max_parallel_calls(&self) -> usize {
        self.max_parallel_calls.load(Ordering::Acquire).max(1)
    }
}

/// Injected host-process execution lanes and background process board.
#[derive(Default)]
pub(super) struct ProcessExecutionLanes {
    pub(super) low_risk_runner: Option<Arc<dyn ProcessRunner>>,
    pub(super) read_only_shell_runner: Option<Arc<dyn ProcessRunner>>,
    pub(super) accepted_local_workspace_runner: Option<AcceptedLocalWorkspaceProcessRunner>,
    pub(super) permissioned_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    pub(super) background_control: Option<Arc<dyn BackgroundProcessControl>>,
}

/// Permission trust level, review mode, rules, and decision history.
pub(super) struct PermissionGovernanceState {
    pub(super) trust_level: RuntimeTrustLevel,
    pub(super) review_mode: RwLock<PermissionReviewMode>,
    pub(super) review_rules: PermissionReviewRules,
    pub(super) review_workspace_root: Option<PathBuf>,
    pub(super) actor: PermissionActor,
    pub(super) recorded_decisions: RwLock<VecDeque<RecordedPermissionDecision>>,
    pub(super) admission_source: RwLock<Option<Arc<dyn PermissionAdmissionSource>>>,
}

impl PermissionGovernanceState {
    pub(super) async fn recorded_decisions_snapshot(&self) -> Vec<RecordedPermissionDecision> {
        self.recorded_decisions
            .read()
            .await
            .iter()
            .cloned()
            .collect()
    }

    pub(super) async fn record_decision(
        &self,
        request: &PermissionRequest,
        decision: &PermissionAdmissionDecision,
    ) {
        let mut decisions = self.recorded_decisions.write().await;
        if decisions.len() >= MAX_RECORDED_PERMISSION_DECISIONS {
            decisions.pop_front();
        }
        decisions.push_back(RecordedPermissionDecision::from_request_and_decision(
            request, decision,
        ));
    }
}

/// Managed subagent orchestration handles and event/activity projections.
pub(super) struct SubagentState {
    pub(super) manager: Option<SubagentManager>,
    pub(super) activity_hub: Arc<SubagentActivityHub>,
    pub(super) event_sink: SubagentEventSink,
}

pub(super) struct RuntimeInner {
    pub(super) session_id: SessionId,
    pub(super) session: Arc<Mutex<SessionState>>,
    pub(super) active_step: Arc<AtomicBool>,
    pub(super) closed: AtomicBool,
    pub(super) event_buffer_size: NonZeroUsize,
    pub(super) capabilities: RuntimeCapabilities,
    pub(super) trajectory: Arc<RuntimeObservability>,
    pub(super) persistence: SessionPersistenceState,
    pub(super) models: ModelContextState,
    pub(super) memory: MemoryState,
    pub(super) tools: ToolDispatchState,
    pub(super) process_lanes: ProcessExecutionLanes,
    pub(super) permissions: PermissionGovernanceState,
    pub(super) subagents: SubagentState,
}

impl RuntimeInner {
    pub(super) async fn model_config(&self, role: RuntimeModelRole) -> Option<ModelProviderConfig> {
        self.models.config(role).await
    }

    pub(super) async fn model_config_with_primary_fallback(
        &self,
        role: RuntimeModelRole,
    ) -> Option<ModelProviderConfig> {
        self.models.config_with_primary_fallback(role).await
    }

    pub(super) fn visible_tool_specs(&self) -> Vec<merry_core::ToolSpec> {
        self.tools.visible_specs()
    }
}

pub(super) struct ToolBatchScope<'a> {
    tools: &'a ToolDispatchState,
}

impl Drop for ToolBatchScope<'_> {
    fn drop(&mut self) {
        debug_assert!(self.tools.batch_active.swap(false, Ordering::AcqRel));
    }
}

#[derive(Clone)]
pub(super) struct AcceptedLocalWorkspaceProcessRunner {
    pub(super) admission: AcceptedLocalWorkspaceProcessAdmission,
    pub(super) runner: Arc<dyn ProcessRunner>,
}
