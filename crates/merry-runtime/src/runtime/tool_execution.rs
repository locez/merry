use crate::{
    ActionExecutionEvidence, ActionProposal, ArtifactContent, ProcessPermissionProfileId,
    RuntimeError, RuntimeModelRole,
    action_audit::ActionAuditPolicy,
    action_policy::{
        ActionPolicyDecision, DefaultActionPolicy, classify_tool_action_risk,
        is_local_workspace_effect_process_action_proposal, is_low_risk_process_action_proposal,
        is_low_risk_workspace_patch_proposal, is_read_only_shell_process_action_proposal,
    },
    permission::{
        ModelBackedPermissionAdmissionSource, PermissionAdmissionContext,
        PermissionAdmissionResult, PermissionAdmissionSource, PermissionedAction,
        is_request_permissions_tool, permission_blocked_outcome, permission_denied_outcome,
        permission_invalid_arguments_outcome, permission_request_from_call,
        permission_review_error_outcome,
    },
    tool::{ActionProposalEvidence, ToolActionPreflight, ToolExecutionContext, ToolExecutionError},
};
use merry_core::{
    CoreError, PendingToolCall, RuntimeEvent, RuntimeEventKind, SessionId, ToolCallId,
    ToolCallResultStatus,
};
use std::sync::Arc;

use super::process_execution::execute_admitted_process_action;
use super::{
    DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED, DIAGNOSTIC_TOOL_NOT_REGISTERED, RuntimeInner,
    TOOL_ACTION_POLICY_DENIED_MESSAGE, WORKSPACE_PATCH_TOOL_NAME, diagnostic_from_text,
};

pub(super) async fn execute_tool_call_with_active_permit(
    inner: &Arc<RuntimeInner>,
    call_id: &ToolCallId,
    context: ToolExecutionContext,
) -> Result<Vec<RuntimeEvent>, RuntimeError> {
    if context.cancellation_token().is_cancelled() {
        return Err(RuntimeError::ToolExecutionCancelled {
            session_id: inner.session_id.clone(),
            call_id: call_id.clone(),
        });
    }

    let pending = {
        let session = inner.session.lock().await;
        session
            .pending_tool_call(call_id)
            .ok_or_else(|| RuntimeError::UnknownToolCall {
                session_id: inner.session_id.clone(),
                call_id: call_id.clone(),
            })?
    };

    let Some(registered_tool) = inner.tool_registry.registered_tool(pending.name()) else {
        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }

        let diagnostic = diagnostic_from_text(
            DIAGNOSTIC_TOOL_NOT_REGISTERED,
            format!("tool {} is not registered", pending.name()),
        );
        let content = ArtifactContent::json(format!(
            r#"{{"error":"tool_not_registered","tool":"{}"}}"#,
            pending.name()
        ));
        let mut session = inner.session.lock().await;
        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }
        return session.submit_tool_execution_outcome(
            call_id,
            ToolCallResultStatus::Failed,
            content,
            Some(diagnostic),
            None,
        );
    };

    if is_request_permissions_tool(pending.name())
        && registered_tool.action_kind() == crate::ToolActionKind::RuntimeControl
    {
        return execute_permission_request_tool_call(inner, &pending, context).await;
    }

    let mut policy_decision = DefaultActionPolicy.decide(registered_tool.action_kind());
    let mut allowed_proposal = None;
    if !policy_decision.is_allowed() {
        let proposal = if registered_tool.action_kind().is_mutating()
            && registered_tool.proposals_enabled()
        {
            if context.cancellation_token().is_cancelled() {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }

            let proposer = registered_tool.executor();
            let proposed = tokio::select! {
                biased;
                () = context.cancellation_token().cancelled() => {
                    return Err(RuntimeError::ToolExecutionCancelled {
                        session_id: inner.session_id.clone(),
                        call_id: call_id.clone(),
                    });
                }
                proposed = proposer.propose(pending.clone(), context.clone()) => proposed,
            };

            if context.cancellation_token().is_cancelled() {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }

            match proposed {
                Ok(ToolActionPreflight::Proposal(proposal)) => {
                    validate_action_proposal(
                        &proposal,
                        &pending,
                        registered_tool.action_kind(),
                        &inner.session_id,
                    )?;
                    Some(proposal)
                }
                Ok(ToolActionPreflight::NoProposal) => None,
                Ok(ToolActionPreflight::Outcome(outcome)) => {
                    let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
                    if status != ToolCallResultStatus::Failed {
                        return Err(RuntimeError::Core {
                            source: CoreError::InvalidToolCallResult {
                                reason: "preflight tool outcome must be failed",
                            },
                        });
                    }
                    debug_assert!(execution_evidence.is_none());
                    let mut session = inner.session.lock().await;
                    if context.cancellation_token().is_cancelled() {
                        return Err(RuntimeError::ToolExecutionCancelled {
                            session_id: inner.session_id.clone(),
                            call_id: call_id.clone(),
                        });
                    }
                    return session
                        .submit_tool_execution_outcome(call_id, status, content, diagnostic, None);
                }
                Err(ToolExecutionError::Cancelled) => {
                    return Err(RuntimeError::ToolExecutionCancelled {
                        session_id: inner.session_id.clone(),
                        call_id: call_id.clone(),
                    });
                }
                Err(ToolExecutionError::Infrastructure { message }) => {
                    return Err(RuntimeError::ToolExecutionFailed {
                        session_id: inner.session_id.clone(),
                        call_id: call_id.clone(),
                        message,
                    });
                }
            }
        } else {
            None
        };

        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }

        if let Some(proposal) = proposal {
            if inner.allow_low_risk_workspace_patches
                && pending.name().as_str() == WORKSPACE_PATCH_TOOL_NAME
                && is_low_risk_workspace_patch_proposal(registered_tool.action_kind(), &proposal)
            {
                policy_decision = ActionPolicyDecision::allow_low_risk_workspace_patch();
                allowed_proposal = Some(proposal);
            } else if let Some(runner) = inner.low_risk_process_runner.clone()
                && is_low_risk_process_action_proposal(registered_tool.action_kind(), &proposal)
            {
                policy_decision = ActionPolicyDecision::allow_low_risk_process_action();
                return execute_admitted_process_action(
                    inner,
                    &pending,
                    proposal,
                    policy_decision,
                    ProcessPermissionProfileId::READ_ONLY_V1,
                    runner,
                    context,
                )
                .await;
            } else if let Some(runner) = inner.read_only_shell_process_runner.clone()
                && is_read_only_shell_process_action_proposal(
                    registered_tool.action_kind(),
                    &proposal,
                )
            {
                policy_decision = ActionPolicyDecision::allow_read_only_shell_process_action();
                return execute_admitted_process_action(
                    inner,
                    &pending,
                    proposal,
                    policy_decision,
                    ProcessPermissionProfileId::SHELL_READ_ONLY_V1,
                    runner,
                    context,
                )
                .await;
            } else if let Some(accepted) = inner.accepted_local_workspace_process_runner.clone()
                && is_local_workspace_effect_process_action_proposal(
                    registered_tool.action_kind(),
                    &proposal,
                    accepted.admission,
                )
            {
                policy_decision =
                    ActionPolicyDecision::allow_accepted_local_workspace_process_action();
                return execute_admitted_process_action(
                    inner,
                    &pending,
                    proposal,
                    policy_decision,
                    accepted.admission.permission_profile_id(),
                    accepted.runner,
                    context,
                )
                .await;
            } else {
                let outcome = denied_tool_action_outcome(&pending);
                let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
                debug_assert_eq!(status, ToolCallResultStatus::Failed);
                debug_assert!(execution_evidence.is_none());
                let diagnostic = diagnostic.ok_or(RuntimeError::Core {
                    source: CoreError::InvalidToolCallResult {
                        reason: "denied tool action outcome must include a diagnostic",
                    },
                })?;
                let mut session = inner.session.lock().await;
                if context.cancellation_token().is_cancelled() {
                    return Err(RuntimeError::ToolExecutionCancelled {
                        session_id: inner.session_id.clone(),
                        call_id: call_id.clone(),
                    });
                }
                let denied_policy_decision = policy_decision.with_risk_tier(
                    classify_tool_action_risk(registered_tool.action_kind(), Some(&proposal)),
                );
                let events = session.submit_denied_tool_action(
                    &pending,
                    &denied_policy_decision,
                    Some(proposal),
                    content,
                    diagnostic,
                )?;
                trace_denied_tool_execution(inner.session_id.as_str(), &pending, &events);
                return Ok(events);
            }
        } else {
            let outcome = denied_tool_action_outcome(&pending);
            let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
            debug_assert_eq!(status, ToolCallResultStatus::Failed);
            debug_assert!(execution_evidence.is_none());
            let diagnostic = diagnostic.ok_or(RuntimeError::Core {
                source: CoreError::InvalidToolCallResult {
                    reason: "denied tool action outcome must include a diagnostic",
                },
            })?;
            let mut session = inner.session.lock().await;
            if context.cancellation_token().is_cancelled() {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }
            let denied_policy_decision = policy_decision.with_risk_tier(classify_tool_action_risk(
                registered_tool.action_kind(),
                None,
            ));
            let events = session.submit_denied_tool_action(
                &pending,
                &denied_policy_decision,
                None,
                content,
                diagnostic,
            )?;
            trace_denied_tool_execution(inner.session_id.as_str(), &pending, &events);
            return Ok(events);
        }
    }

    if let Err(error) = admit_action_to_generic_executor(
        &pending,
        registered_tool.action_kind(),
        &policy_decision,
        allowed_proposal.as_ref(),
        &inner.session_id,
    ) {
        let mut session = inner.session.lock().await;
        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }
        session.record_guarded_tool_action(
            &pending,
            registered_tool.action_kind(),
            ActionAuditPolicy::from_decision(&policy_decision),
        )?;
        return Err(error);
    }

    let execution_context =
        context_with_approved_proposal(context.clone(), allowed_proposal.as_ref());
    let executor = registered_tool.executor();
    let preserves_control_state =
        registered_tool.action_kind() == crate::ToolActionKind::RuntimeControl;
    let execution = if allowed_proposal.is_some() || preserves_control_state {
        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }
        executor.execute(pending, execution_context).await
    } else {
        tokio::select! {
            biased;
            () = context.cancellation_token().cancelled() => {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }
            execution = executor.execute(pending, execution_context) => execution,
        }
    };

    let outcome = match execution {
        Ok(outcome) => {
            if context.cancellation_token().is_cancelled()
                && allowed_proposal.is_none()
                && !preserves_control_state
            {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }
            outcome
        }
        Err(ToolExecutionError::Cancelled) => {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }
        Err(ToolExecutionError::Infrastructure { message }) => {
            if context.cancellation_token().is_cancelled() {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }
            return Err(RuntimeError::ToolExecutionFailed {
                session_id: inner.session_id.clone(),
                call_id: call_id.clone(),
                message,
            });
        }
    };

    let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
    let mut session = inner.session.lock().await;
    if context.cancellation_token().is_cancelled()
        && allowed_proposal.is_none()
        && !preserves_control_state
    {
        return Err(RuntimeError::ToolExecutionCancelled {
            session_id: inner.session_id.clone(),
            call_id: call_id.clone(),
        });
    }
    if let Some(proposal) = allowed_proposal {
        if status == ToolCallResultStatus::Succeeded && execution_evidence.is_none() {
            return Err(RuntimeError::MissingActionExecutionEvidence {
                session_id: inner.session_id.clone(),
                call_id: call_id.clone(),
                action_kind: registered_tool.action_kind(),
            });
        }
        if status == ToolCallResultStatus::Succeeded
            && let Some(evidence) = execution_evidence.as_ref()
        {
            if !evidence.matches_action_kind(registered_tool.action_kind()) {
                return Err(RuntimeError::ToolExecutionFailed {
                    session_id: inner.session_id.clone(),
                    call_id: call_id.clone(),
                    message: "admitted action execution evidence did not match the registered action kind"
                        .to_owned(),
                });
            }
            if !action_execution_evidence_matches_proposal(&proposal, evidence) {
                return Err(RuntimeError::ToolExecutionFailed {
                    session_id: inner.session_id.clone(),
                    call_id: call_id.clone(),
                    message:
                        "admitted workspace patch execution evidence did not match the approved proposal"
                            .to_owned(),
                });
            }
        }
        return session.submit_proposed_tool_execution_outcome(
            proposal,
            status,
            content,
            diagnostic,
            execution_evidence,
            ActionAuditPolicy::from_decision(&policy_decision),
        );
    }
    session.submit_tool_execution_outcome(call_id, status, content, diagnostic, execution_evidence)
}

async fn execute_permission_request_tool_call(
    inner: &Arc<RuntimeInner>,
    pending: &PendingToolCall,
    context: ToolExecutionContext,
) -> Result<Vec<RuntimeEvent>, RuntimeError> {
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
            let mut session = inner.session.lock().await;
            return session.submit_tool_execution_outcome(
                pending.id(),
                status,
                content,
                diagnostic,
                None,
            );
        }
    };

    let Some(runner_factory) = inner.permissioned_process_runner_factory.clone() else {
        let outcome = permission_blocked_outcome(
            pending,
            "permissioned process execution is not configured for this runtime",
        );
        let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
        debug_assert!(execution_evidence.is_none());
        let mut session = inner.session.lock().await;
        return session.submit_tool_execution_outcome(
            pending.id(),
            status,
            content,
            diagnostic,
            None,
        );
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
            let mut session = inner.session.lock().await;
            return session.submit_tool_execution_outcome(
                pending.id(),
                status,
                content,
                diagnostic,
                None,
            );
        }
    };

    if !decision.is_approved() {
        let outcome = permission_denied_outcome(pending, Some(decision.review()));
        let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
        debug_assert!(execution_evidence.is_none());
        let mut session = inner.session.lock().await;
        return session.submit_tool_execution_outcome(
            pending.id(),
            status,
            content,
            diagnostic,
            None,
        );
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
        ActionPolicyDecision::allow_permissioned_process_action(),
        ProcessPermissionProfileId::APPROVED_PERMISSION_REQUEST_V1,
        runner,
        context,
    )
    .await
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
            .model_configs
            .get_with_primary_fallback(RuntimeModelRole::ApprovalReview)
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

pub(super) fn denied_tool_action_outcome(pending: &PendingToolCall) -> crate::ToolExecutionOutcome {
    let diagnostic = diagnostic_from_text(
        DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
        TOOL_ACTION_POLICY_DENIED_MESSAGE,
    );
    let payload = serde_json::json!({
        "ok": false,
        "tool": pending.name().as_str(),
        "error": {
            "code": DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
            "message": TOOL_ACTION_POLICY_DENIED_MESSAGE
        }
    });

    crate::ToolExecutionOutcome::failed_json(payload.to_string(), diagnostic)
}

pub(super) fn trace_denied_tool_execution(
    session_id: &str,
    pending: &PendingToolCall,
    events: &[RuntimeEvent],
) {
    tracing::info!(
        event = "runtime.tool.execute.finish",
        session_id,
        tool_call_id = pending.id().as_str(),
        tool_name = pending.name().as_str(),
        status = "denied",
        artifact_id = tool_resolution_artifact_id(events),
        diagnostic_code = DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
        "runtime tool execution denied"
    );
}

fn tool_resolution_artifact_id(events: &[RuntimeEvent]) -> String {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => {
                Some(result.artifact().id().as_str().to_owned())
            }
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn validate_action_proposal(
    proposal: &ActionProposal,
    pending: &PendingToolCall,
    action_kind: crate::ToolActionKind,
    session_id: &SessionId,
) -> Result<(), RuntimeError> {
    proposal
        .validate_for_call(pending, action_kind)
        .map_err(|reason| RuntimeError::ToolExecutionFailed {
            session_id: session_id.clone(),
            call_id: pending.id().clone(),
            message: reason.to_owned(),
        })
}

pub(super) fn context_with_approved_proposal(
    context: ToolExecutionContext,
    proposal: Option<&ActionProposal>,
) -> ToolExecutionContext {
    match proposal.map(ActionProposal::evidence) {
        Some(ActionProposalEvidence::WorkspacePatch(patch)) => {
            context.with_approved_workspace_patch(patch.clone())
        }
        Some(ActionProposalEvidence::ProcessAction(_)) | None => context,
    }
}

pub(super) fn action_execution_evidence_matches_proposal(
    proposal: &ActionProposal,
    execution_evidence: &ActionExecutionEvidence,
) -> bool {
    match (proposal.evidence(), execution_evidence) {
        (
            ActionProposalEvidence::WorkspacePatch(proposed),
            ActionExecutionEvidence::WorkspacePatch(executed),
        ) => proposed.changes() == executed.changes(),
        (
            ActionProposalEvidence::ProcessAction(proposed),
            ActionExecutionEvidence::ProcessAction(executed),
        ) => executed.matches_intent(proposed),
        _ => false,
    }
}

pub(super) fn admit_action_to_generic_executor(
    pending: &PendingToolCall,
    action_kind: crate::ToolActionKind,
    decision: &ActionPolicyDecision,
    proposal: Option<&ActionProposal>,
    session_id: &SessionId,
) -> Result<(), RuntimeError> {
    if !action_kind.is_mutating() {
        return Ok(());
    }

    let low_risk_workspace_patch_admitted = decision.is_allowed()
        && decision.action_kind() == crate::ToolActionKind::WorkspaceWrite
        && decision.risk_tier() == crate::action_policy::ActionRiskTier::EditLow
        && pending.name().as_str() == WORKSPACE_PATCH_TOOL_NAME
        && action_kind == crate::ToolActionKind::WorkspaceWrite
        && proposal
            .is_some_and(|proposal| is_low_risk_workspace_patch_proposal(action_kind, proposal));

    if !low_risk_workspace_patch_admitted {
        return Err(RuntimeError::MutatingActionCommitLifecycleRequired {
            session_id: session_id.clone(),
            call_id: pending.id().clone(),
            action_kind,
        });
    }

    Ok(())
}
