use super::process_execution::{ProcessExecutionAdmission, execute_admitted_process_action};
use super::{RuntimeInner, persist_resume_safe_savepoint_if_configured};
use crate::{
    ActionProposal, ProcessPermissionProfileId, RuntimeError, RuntimeModelRole,
    action_policy::ActionPolicyDecision,
    permission::{
        ModelBackedPermissionAdmissionSource, PermissionAdmissionContext,
        PermissionAdmissionResult, PermissionAdmissionSource, PermissionedAction,
        permission_blocked_outcome, permission_denied_outcome,
        permission_invalid_arguments_outcome, permission_request_from_call,
        permission_review_error_outcome,
    },
    tool::{ActionProposalEvidence, ToolExecutionContext},
};
use merry_core::{PendingToolCall, RuntimeJournalEvent};
use std::sync::Arc;

pub(super) async fn execute_permission_request_tool_call(
    inner: &Arc<RuntimeInner>,
    pending: &PendingToolCall,
    context: ToolExecutionContext,
    attribute_plan_effect: bool,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    if context.cancellation_token().is_cancelled() {
        return Err(RuntimeError::ToolExecutionCancelled {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
        });
    }

    let review_context = {
        let session = inner.session.lock().await;
        session.permission_review_context_snapshot()?
    };
    let request = match permission_request_from_call(pending, review_context) {
        Ok(request) => request,
        Err(error) => {
            let outcome = permission_invalid_arguments_outcome(pending.name().as_str(), error);
            let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
            debug_assert!(execution_evidence.is_none());
            let events = {
                let mut session = inner.session.lock().await;
                session.submit_tool_execution_outcome(
                    pending.id(),
                    status,
                    content,
                    diagnostic,
                    None,
                )?
            };
            return persist_tool_events(inner, events).await;
        }
    };

    let Some(runner_factory) = inner.permissioned_process_runner_factory.clone() else {
        let outcome = permission_blocked_outcome(
            pending,
            "permissioned process execution is not configured for this runtime",
        );
        let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
        debug_assert!(execution_evidence.is_none());
        let events = {
            let mut session = inner.session.lock().await;
            session.submit_tool_execution_outcome(
                pending.id(),
                status,
                content,
                diagnostic,
                None,
            )?
        };
        return persist_tool_events(inner, events).await;
    };

    let admission = review_permission_request(inner, request.clone(), &context).await;
    let decision = match admission {
        Ok(decision) => decision,
        Err(error) => {
            if matches!(error, crate::PermissionAdmissionError::Cancelled)
                || context.cancellation_token().is_cancelled()
            {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: inner.session_id.clone(),
                    call_id: pending.id().clone(),
                });
            }
            let outcome = permission_review_error_outcome(pending, &error);
            let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
            debug_assert!(execution_evidence.is_none());
            let events = {
                let mut session = inner.session.lock().await;
                session.submit_tool_execution_outcome(
                    pending.id(),
                    status,
                    content,
                    diagnostic,
                    None,
                )?
            };
            return persist_tool_events(inner, events).await;
        }
    };

    if !decision.is_approved() {
        let outcome = permission_denied_outcome(pending, Some(decision.review()));
        let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
        debug_assert!(execution_evidence.is_none());
        let events = {
            let mut session = inner.session.lock().await;
            session.submit_tool_execution_outcome(
                pending.id(),
                status,
                content,
                diagnostic,
                None,
            )?
        };
        return persist_tool_events(inner, events).await;
    }

    let runner = runner_factory.runner_for(&request);
    let PermissionedAction::Process(intent) = request.action().clone();
    let proposal = ActionProposal::new(
        pending,
        crate::ToolActionKind::CommandExec,
        "permissioned process",
        "approved permission request",
        format!(
            "Run process with {} argv item(s) after permission admission review",
            intent.argv().len()
        ),
        ActionProposalEvidence::ProcessAction(intent),
    )
    .map_err(|error| RuntimeError::ToolExecutionFailed {
        session_id: inner.session_id.clone(),
        call_id: pending.id().clone(),
        message: error.to_string(),
    })?;
    execute_admitted_process_action(
        inner,
        pending,
        proposal,
        ProcessExecutionAdmission::new(
            ActionPolicyDecision::allow_permissioned_process_action(),
            ProcessPermissionProfileId::APPROVED_PERMISSION_REQUEST_V1,
            runner,
            attribute_plan_effect,
        ),
        context,
    )
    .await
}

async fn persist_tool_events(
    inner: &RuntimeInner,
    events: Vec<RuntimeJournalEvent>,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    persist_resume_safe_savepoint_if_configured(inner).await;
    Ok(events)
}

async fn review_permission_request(
    inner: &RuntimeInner,
    request: crate::PermissionRequest,
    context: &ToolExecutionContext,
) -> PermissionAdmissionResult {
    if inner
        .permission_review_mode
        .requires_model_review(inner.runtime_trust_level)
    {
        let Some(model_config) = inner
            .model_config_with_primary_fallback(RuntimeModelRole::ApprovalReview)
            .await
        else {
            return Err(crate::PermissionAdmissionError::ReviewModelUnavailable);
        };
        let source = ModelBackedPermissionAdmissionSource::from_config(model_config)?;
        return source
            .review(
                request,
                PermissionAdmissionContext::new(context.cancellation_token().clone()),
            )
            .await;
    }

    let Some(source) = inner.permission_admission_source.as_ref() else {
        return Err(crate::PermissionAdmissionError::ReviewModelUnavailable);
    };
    source
        .review(
            request,
            PermissionAdmissionContext::new(context.cancellation_token().clone()),
        )
        .await
}
