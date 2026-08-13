use super::process_execution::{ProcessExecutionAdmission, execute_admitted_process_action};
use super::{RuntimeInner, persist_resume_safe_savepoint_if_configured};
use crate::{
    ActionProposal, ProcessPermissionProfileId, RuntimeError, RuntimeModelRole,
    action_policy::ActionPolicyDecision,
    permission::{
        ModelBackedPermissionAdmissionSource, PermissionAdmissionContext,
        PermissionAdmissionResult, PermissionAdmissionReview, PermissionAdmissionSource,
        PermissionRequest, PermissionedAction, permission_blocked_outcome,
        permission_denied_outcome, permission_invalid_arguments_outcome,
        permission_request_from_call, permission_review_error_outcome,
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
            None,
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

    if let Err(error) = runner_factory.validate_request(&request) {
        let outcome = permission_blocked_outcome(pending, &error.to_string(), Some(&request));
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
            let outcome = permission_review_error_outcome(pending, &request, &error);
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
        let outcome = permission_denied_outcome(pending, &request, Some(decision.review()));
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
        "Run the exact shell command after permission admission review",
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
        )
        .with_permission_review(decision.review().clone()),
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

/// Result of reviewing a high-risk action without changing session grants.
pub(super) enum HighRiskActionReview {
    /// The action may continue through its already-selected process runner.
    Approved(PermissionAdmissionReview),
    /// The original tool call was durably resolved as denied or review-failed.
    Resolved(Vec<RuntimeJournalEvent>),
}

/// Reviews one high-risk process action through the normal permission source.
///
/// This is separate from `request_permissions`: an action review has no
/// requested capability and therefore cannot grant or retain a path, network,
/// or host-integration capability as a side effect.
pub(super) async fn review_high_risk_process_action(
    inner: &Arc<RuntimeInner>,
    pending: &PendingToolCall,
    proposal: &ActionProposal,
    context: &ToolExecutionContext,
) -> Result<HighRiskActionReview, RuntimeError> {
    if context.cancellation_token().is_cancelled() {
        return Err(RuntimeError::ToolExecutionCancelled {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
        });
    }

    let crate::tool::ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
        return Err(RuntimeError::ToolExecutionFailed {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
            message: "high-risk process review received a non-process proposal".to_owned(),
        });
    };
    let review_context = {
        let session = inner.session.lock().await;
        session.permission_review_context_snapshot()?
    };
    let request = PermissionRequest::for_action_review(
        pending,
        "high-risk process action requires an independent action review",
        PermissionedAction::Process(intent.clone()),
        review_context,
    );
    let admission = review_permission_request(inner, request.clone(), context).await;
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
            let outcome = permission_review_error_outcome(pending, &request, &error);
            let (_status, content, diagnostic, execution_evidence) = outcome.into_parts();
            debug_assert!(execution_evidence.is_none());
            let diagnostic = diagnostic.ok_or(RuntimeError::Core {
                source: merry_core::CoreError::InvalidToolCallResult {
                    reason: "permission review failure must include a diagnostic",
                },
            })?;
            let events = {
                let mut session = inner.session.lock().await;
                session.submit_denied_tool_action(
                    pending,
                    &crate::action_policy::ActionPolicyDecision::deny_high_risk_process_action(),
                    Some(proposal.clone()),
                    content,
                    diagnostic,
                )?
            };
            super::tool_execution::trace_review_denied_tool_execution(
                inner.session_id.as_str(),
                pending,
                &events,
                "review_failed",
            );
            return Ok(HighRiskActionReview::Resolved(
                persist_tool_events(inner, events).await?,
            ));
        }
    };

    if !decision.is_approved() {
        let outcome = permission_denied_outcome(pending, &request, Some(decision.review()));
        let (_status, content, diagnostic, execution_evidence) = outcome.into_parts();
        debug_assert!(execution_evidence.is_none());
        let diagnostic = diagnostic.ok_or(RuntimeError::Core {
            source: merry_core::CoreError::InvalidToolCallResult {
                reason: "permission denial must include a diagnostic",
            },
        })?;
        let events = {
            let mut session = inner.session.lock().await;
            session.submit_denied_tool_action(
                pending,
                &crate::action_policy::ActionPolicyDecision::deny_high_risk_process_action(),
                Some(proposal.clone()),
                content,
                diagnostic,
            )?
        };
        super::tool_execution::trace_review_denied_tool_execution(
            inner.session_id.as_str(),
            pending,
            &events,
            "denied",
        );
        return Ok(HighRiskActionReview::Resolved(
            persist_tool_events(inner, events).await?,
        ));
    }

    Ok(HighRiskActionReview::Approved(decision.review().clone()))
}

pub(super) async fn review_permission_request(
    inner: &RuntimeInner,
    request: crate::PermissionRequest,
    context: &ToolExecutionContext,
) -> PermissionAdmissionResult {
    let mode = inner.permission_review_mode;
    if matches!(mode, crate::PermissionReviewMode::NonInteractiveTrusted) {
        return Ok(crate::PermissionAdmissionDecision::approved(
            "explicit non-interactive trusted mode admitted this configured action",
        ));
    }

    if mode.requires_model_review(inner.runtime_trust_level) {
        let Some(model_config) = inner
            .model_config_with_primary_fallback(RuntimeModelRole::ApprovalReview)
            .await
        else {
            return host_fallback_or_error(inner, mode, request, context, None).await;
        };
        let source = ModelBackedPermissionAdmissionSource::from_config(model_config)?;
        let result = source
            .review(
                request.clone(),
                PermissionAdmissionContext::new(context.cancellation_token().clone()),
            )
            .await;
        return match result {
            Ok(decision) => Ok(decision),
            Err(error) if matches!(error, crate::PermissionAdmissionError::Cancelled) => Err(error),
            Err(error) if matches!(mode, crate::PermissionReviewMode::ModelThenHostFallback) => {
                match host_fallback_or_error(inner, mode, request, context, Some(error.to_string()))
                    .await
                {
                    Ok(decision) => Ok(decision),
                    Err(fallback_error) => Err(crate::PermissionAdmissionError::ReviewFailed {
                        message: format!(
                            "AI approval review failed: {error}; human fallback failed: {fallback_error}"
                        ),
                    }),
                }
            }
            Err(error) => Err(error),
        };
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

async fn host_fallback_or_error(
    inner: &RuntimeInner,
    mode: crate::PermissionReviewMode,
    request: crate::PermissionRequest,
    context: &ToolExecutionContext,
    review_failure: Option<String>,
) -> PermissionAdmissionResult {
    if !matches!(mode, crate::PermissionReviewMode::ModelThenHostFallback) {
        return Err(crate::PermissionAdmissionError::ReviewModelUnavailable);
    }
    let Some(source) = inner.permission_admission_source.as_ref() else {
        return Err(crate::PermissionAdmissionError::ReviewModelUnavailable);
    };
    let review_context = PermissionAdmissionContext::new(context.cancellation_token().clone());
    let review_context = review_failure.map_or(review_context.clone(), |failure| {
        review_context.with_review_failure(failure)
    });
    source.review(request, review_context).await
}
