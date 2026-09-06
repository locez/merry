use super::config::AutomaticCompactionConfig;
use crate::{
    AcceptedLocalWorkspaceProcessAdmission, FileSessionStore, ProcessRunner, RuntimeCapabilities,
    RuntimeModelRole,
    memory::MemoryActivationSource,
    model_config::{ModelProviderConfig, RuntimeModelConfigs},
    permission::{PermissionAdmissionSource, PermissionReviewMode, RuntimeTrustLevel},
    plan::{PlanController, PlanControllerError, PlanSubagentControl},
    process::PermissionedProcessRunnerFactory,
    session::SessionState,
    subagent::{PlanSubagentScope, SubagentActivityHub, SubagentManager},
    tool::ToolRegistry,
    trajectory::RuntimeObservability,
};
use merry_core::{PlanHarnessSnapshot, SessionId};
use std::{
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::{Mutex, RwLock};

pub(super) struct RuntimeInner {
    pub(super) session_id: SessionId,
    pub(super) session: Arc<Mutex<SessionState>>,
    pub(super) active_step: Arc<AtomicBool>,
    pub(super) memory_projection_epoch: AtomicU64,
    pub(super) event_buffer_size: NonZeroUsize,
    pub(super) max_parallel_tool_calls: NonZeroUsize,
    pub(super) model_configs: RuntimeModelConfigs,
    pub(super) primary_model_override: RwLock<Option<ModelProviderConfig>>,
    pub(super) automatic_compaction: RwLock<AutomaticCompactionConfig>,
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
    pub(super) permission_review_mode: PermissionReviewMode,
    pub(super) permission_admission_source: Option<Arc<dyn PermissionAdmissionSource>>,
    pub(super) permissioned_process_runner_factory:
        Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    pub(super) subagent_manager: Option<SubagentManager>,
    pub(super) coordinator_plan_tools: bool,
    pub(super) plan_controller: PlanController,
    pub(super) plan_subagent_control: Option<PlanSubagentControl>,
    pub(super) plan_subagent_scope: Option<PlanSubagentScope>,
    pub(super) session_store: Option<FileSessionStore>,
    pub(super) tool_batch_active: AtomicBool,
    pub(super) activity_hub: Arc<SubagentActivityHub>,
    pub(super) trajectory: Arc<RuntimeObservability>,
}

impl RuntimeInner {
    pub(super) fn begin_tool_batch(&self) -> ToolBatchScope<'_> {
        debug_assert!(!self.tool_batch_active.swap(true, Ordering::AcqRel));
        ToolBatchScope { inner: self }
    }

    pub(super) fn tool_batch_active(&self) -> bool {
        self.tool_batch_active.load(Ordering::Acquire)
    }

    pub(super) async fn active_subagent_plan_harness(
        &self,
    ) -> Result<PlanHarnessSnapshot, PlanControllerError> {
        self.plan_subagent_control
            .as_ref()
            .expect("subagent harness is requested only for a bound runtime")
            .active_harness()
            .await
    }

    pub(super) async fn record_plan_runtime_effect(
        &self,
        changed_paths: Vec<String>,
    ) -> Result<(), PlanControllerError> {
        let now_ms = crate::plan::unix_time_ms();
        if let Some(control) = self.plan_subagent_control.as_ref() {
            control
                .record_runtime_effect(changed_paths, now_ms)
                .await
                .map(|_| ())
        } else {
            self.plan_controller
                .record_runtime_effect(
                    crate::plan::execution::PlanAttemptActor {
                        executor_session_id: self.session_id.clone(),
                    },
                    changed_paths,
                    now_ms,
                )
                .await
                .map(|_| ())
        }
    }

    pub(super) async fn model_config(&self, role: RuntimeModelRole) -> Option<ModelProviderConfig> {
        if role == RuntimeModelRole::Primary
            && let Some(config) = self.primary_model_override.read().await.as_ref()
        {
            return Some(config.clone());
        }
        self.model_configs.get(role)
    }

    pub(super) async fn model_config_with_primary_fallback(
        &self,
        role: RuntimeModelRole,
    ) -> Option<ModelProviderConfig> {
        if role != RuntimeModelRole::Primary
            && let Some(config) = self.model_configs.get(role)
        {
            return Some(config);
        }
        self.model_config(RuntimeModelRole::Primary).await
    }

    pub(super) fn visible_tool_specs(&self) -> Vec<merry_core::ToolSpec> {
        self.tool_registry.tool_specs()
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
