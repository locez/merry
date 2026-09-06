use crate::plan::{PlanController, SubagentPlanUpdateInput};
use futures_util::future::BoxFuture;
use merry_core::{PlanBindingId, PlanLinkSnapshot, PlanLinkStatus, SubagentId, SubagentTaskId};
use std::sync::Arc;

#[derive(Clone)]
pub struct PlanSubagentScope(crate::plan::PlanSubagentScope);

impl PlanSubagentScope {
    pub(crate) fn from_internal(scope: crate::plan::PlanSubagentScope) -> Self {
        Self(scope)
    }

    pub(crate) async fn read(
        &self,
    ) -> Result<merry_core::PlanSnapshot, crate::PlanControllerError> {
        self.0.read().await
    }

    pub(crate) async fn update_plan(
        &self,
        input: SubagentPlanUpdateInput,
    ) -> Result<crate::PlanUpdateOutput, crate::PlanControllerError> {
        self.0.update_plan(input).await
    }
}
pub trait PlanLinkRuntime: Send + Sync {
    /// Binds a newly allocated subagent to a Plan node identified by client key.
    fn bind_subagent<'a>(
        &'a self,
        client_key: String,
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        now_ms: u64,
    ) -> BoxFuture<'a, Result<PlanLinkSnapshot, String>>;

    /// Updates the runtime-derived terminal state for an existing link.
    fn update_subagent_link<'a>(
        &'a self,
        binding_id: PlanBindingId,
        status: PlanLinkStatus,
        now_ms: u64,
    ) -> BoxFuture<'a, Result<(), String>>;

    /// Creates a scoped Plan capability only for an active, non-superseded link.
    fn scope_for_link<'a>(
        &'a self,
        _link: &'a PlanLinkSnapshot,
    ) -> BoxFuture<'a, Result<Option<PlanSubagentScope>, String>> {
        Box::pin(async { Ok(None) })
    }
}

struct PlanControllerLinkRuntime(PlanController);

impl PlanLinkRuntime for PlanControllerLinkRuntime {
    fn bind_subagent<'a>(
        &'a self,
        client_key: String,
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        now_ms: u64,
    ) -> BoxFuture<'a, Result<PlanLinkSnapshot, String>> {
        Box::pin(async move {
            self.0
                .bind_subagent(client_key, agent_id, task_id, now_ms)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn update_subagent_link<'a>(
        &'a self,
        binding_id: PlanBindingId,
        status: PlanLinkStatus,
        now_ms: u64,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.0
                .update_subagent_link(binding_id, status, now_ms)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }

    fn scope_for_link<'a>(
        &'a self,
        link: &'a PlanLinkSnapshot,
    ) -> BoxFuture<'a, Result<Option<PlanSubagentScope>, String>> {
        Box::pin(async move {
            if link.status != PlanLinkStatus::Active || link.superseded_by.is_some() {
                return Ok(None);
            }
            let Some(snapshot) = self.0.snapshot().await.map_err(|error| error.to_string())? else {
                return Ok(None);
            };
            let Some(current_link) =
                snapshot
                    .nodes
                    .iter()
                    .flat_map(|node| &node.links)
                    .find(|current| {
                        current.plan_id == link.plan_id
                            && current.node_id == link.node_id
                            && current.binding_id == link.binding_id
                            && current.subagent_id == link.subagent_id
                            && current.task_id == link.task_id
                    })
            else {
                return Ok(None);
            };
            if current_link.status != PlanLinkStatus::Active || current_link.superseded_by.is_some()
            {
                return Ok(None);
            }
            Ok(Some(PlanSubagentScope::from_internal(
                self.0.subagent_scope(
                    current_link.plan_id.clone(),
                    current_link.node_id.clone(),
                    current_link.binding_id.clone(),
                ),
            )))
        })
    }
}

pub(crate) fn plan_link_runtime_for_controller(
    controller: PlanController,
) -> Arc<dyn PlanLinkRuntime> {
    Arc::new(PlanControllerLinkRuntime(controller))
}
