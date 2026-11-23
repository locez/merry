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

pub(super) struct RuntimeInner {
    pub(super) session_id: SessionId,
    pub(super) prompt_cache_key: SessionId,
    pub(super) session: Arc<Mutex<SessionState>>,
    pub(super) active_step: Arc<AtomicBool>,
    pub(super) memory_projection_epoch: AtomicU64,
    pub(super) event_buffer_size: NonZeroUsize,
    pub(super) max_parallel_tool_calls: AtomicUsize,
    pub(super) model_configs: RuntimeModelConfigs,
    pub(super) role_model_overrides:
        RwLock<BTreeMap<RuntimeModelRole, Option<ModelProviderConfig>>>,
    pub(super) automatic_compaction: RwLock<CompactionConfig>,
    pub(super) history_retention: HistoryRetention,
    pub(super) context_window_tokens: RwLock<Option<NonZeroU64>>,
    pub(super) capabilities: RuntimeCapabilities,
    pub(super) prompt_profile: crate::PromptProfile,
    pub(super) progress_commentary: bool,
    pub(super) tool_registry: ToolRegistry,
    pub(super) tool_admission: Option<crate::ToolAdmission>,
    pub(super) memory_activation_source: Arc<dyn MemoryActivationSource>,
    pub(super) allow_low_risk_apply_patches: bool,
    pub(super) low_risk_process_runner: Option<Arc<dyn ProcessRunner>>,
    pub(super) read_only_shell_process_runner: Option<Arc<dyn ProcessRunner>>,
    pub(super) accepted_local_workspace_process_runner: Option<AcceptedLocalWorkspaceProcessRunner>,
    pub(super) runtime_trust_level: RuntimeTrustLevel,
    pub(super) permission_review_mode: RwLock<PermissionReviewMode>,
    pub(super) permission_review_rules: PermissionReviewRules,
    pub(super) permission_review_workspace_root: Option<PathBuf>,
    pub(super) permission_actor: PermissionActor,
    pub(super) recorded_permission_decisions: RwLock<VecDeque<RecordedPermissionDecision>>,
    pub(super) permission_admission_source: RwLock<Option<Arc<dyn PermissionAdmissionSource>>>,
    pub(super) permissioned_process_runner_factory:
        Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    pub(super) background_process_control: Option<Arc<dyn BackgroundProcessControl>>,
    pub(super) subagent_manager: Option<SubagentManager>,
    pub(super) coordinator_plan_tools: bool,
    pub(super) session_store: Option<FileSessionStore>,
    pub(super) tool_batch_active: AtomicBool,
    pub(super) activity_hub: Arc<SubagentActivityHub>,
    pub(super) subagent_event_sink: SubagentEventSink,
    pub(super) trajectory: Arc<RuntimeObservability>,
    pub(super) durable_memory: Option<Arc<crate::DurableMemory>>,
    pub(super) memory_maintenance: bool,
    pub(super) memory_context: Option<Arc<str>>,
}

impl RuntimeInner {
    pub(super) fn begin_tool_batch(&self) -> ToolBatchScope<'_> {
        debug_assert!(!self.tool_batch_active.swap(true, Ordering::AcqRel));
        ToolBatchScope { inner: self }
    }

    pub(super) async fn model_config(&self, role: RuntimeModelRole) -> Option<ModelProviderConfig> {
        if let Some(override_entry) = self.role_model_overrides.read().await.get(&role) {
            return override_entry.clone();
        }
        self.model_configs.get(role)
    }

    pub(super) async fn model_config_with_primary_fallback(
        &self,
        role: RuntimeModelRole,
    ) -> Option<ModelProviderConfig> {
        if role != RuntimeModelRole::Primary
            && let Some(config) = self.model_config(role).await
        {
            return Some(config);
        }
        self.model_config(RuntimeModelRole::Primary).await
    }

    pub(super) fn visible_tool_specs(&self) -> Vec<merry_core::ToolSpec> {
        self.tool_registry.tool_specs()
    }

    pub(super) async fn recorded_permission_decisions_snapshot(
        &self,
    ) -> Vec<RecordedPermissionDecision> {
        self.recorded_permission_decisions
            .read()
            .await
            .iter()
            .cloned()
            .collect()
    }

    pub(super) async fn record_permission_decision(
        &self,
        request: &PermissionRequest,
        decision: &PermissionAdmissionDecision,
    ) {
        let mut decisions = self.recorded_permission_decisions.write().await;
        if decisions.len() >= MAX_RECORDED_PERMISSION_DECISIONS {
            decisions.pop_front();
        }
        decisions.push_back(RecordedPermissionDecision::from_request_and_decision(
            request, decision,
        ));
    }
}

pub(super) struct ToolBatchScope<'a> {
    inner: &'a RuntimeInner,
}

impl Drop for ToolBatchScope<'_> {
    fn drop(&mut self) {
        debug_assert!(self.inner.tool_batch_active.swap(false, Ordering::AcqRel));
    }
}

#[derive(Clone)]
pub(super) struct AcceptedLocalWorkspaceProcessRunner {
    pub(super) admission: AcceptedLocalWorkspaceProcessAdmission,
    pub(super) runner: Arc<dyn ProcessRunner>,
}
