use crate::{
    ActionExecutionEvidence, ActionProposal, ProcessPermissionProfileId, RuntimeError, ToolOutput,
    action_policy::{
        ActionPolicyDecision, DefaultActionPolicy, classify_tool_action_risk,
        is_local_workspace_effect_process_action_proposal, is_low_risk_apply_patch_proposal,
        is_low_risk_process_action_proposal, is_read_only_shell_process_action_proposal,
    },
    permission::is_request_permissions_tool,
    plan::tools::is_plan_tool,
    process_tool::process_call_requests_permission,
    runtime::{
        APPLY_PATCH_TOOL_NAME, DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
        DIAGNOSTIC_TOOL_NOT_REGISTERED, RuntimeInner, TOOL_ACTION_POLICY_DENIED_MESSAGE,
        diagnostic_from_text,
        diagnostics::DIAGNOSTIC_TOOL_NOT_ADMITTED,
        permission_execution::{
            HighRiskActionReview, execute_permission_request_tool_call,
            execute_process_permission_request_tool_call, review_process_action,
        },
        plan_tool_execution::execute_plan_or_todo_tool_call,
        process_execution::{ProcessExecutionAdmission, execute_admitted_process_action},
    },
    todo::is_todo_tool,
    tool::{ActionProposalEvidence, ToolActionPreflight, ToolExecutionContext, ToolExecutionError},
};
use merry_core::{
    CoreError, PendingToolCall, RuntimeJournalEvent, SessionId, ToolCallId, ToolCallResult,
    ToolCallResultStatus,
};
use std::sync::Arc;

pub(super) async fn execute_tool_call_with_active_permit(
    inner: &Arc<RuntimeInner>,
    call_id: &ToolCallId,
    context: ToolExecutionContext,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
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

    if inner
        .tool_admission
        .as_ref()
        .is_some_and(|admission| !admission.allows(pending.name()))
    {
        return resolve_tool_admission_denial(inner, &pending, context).await;
    }

    if is_plan_tool(pending.name()) || is_todo_tool(pending.name()) {
        return execute_plan_or_todo_tool_call(inner, &pending).await;
    }

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
        let content = ToolOutput::json(format!(
            r#"{{"error":"tool_not_registered","tool":"{}"}}"#,
            pending.name()
        ));
        let events = {
            let mut session = inner.session.lock().await;
            if context.cancellation_token().is_cancelled() {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }
            session.submit_tool_execution_outcome(
                call_id,
                ToolCallResultStatus::Failed,
                content,
                Some(diagnostic),
                None,
            )?
        };
        return Ok(events);
    };

    if let Some(Err(error)) = inner.tool_registry.validate_tool_input(&pending) {
        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }

        let content = error.content_for_call(&pending);
        let diagnostic = error.diagnostic();
        let events = {
            let mut session = inner.session.lock().await;
            if context.cancellation_token().is_cancelled() {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }
            session.submit_tool_execution_outcome(
                call_id,
                ToolCallResultStatus::Failed,
                content,
                Some(diagnostic),
                None,
            )?
        };
        return Ok(events);
    }

    // Plan Mode read-only edit gate: block workspace file mutations even under
    // fully-trusted permission mode so design remains separate from coding.
    if registered_tool.action_kind() == crate::ToolActionKind::WorkspaceWrite {
        let plan_mode_active = inner.session.lock().await.plan_state().is_active();
        if plan_mode_active {
            return resolve_plan_mode_read_only_denial(inner, &pending).await;
        }
    }

    if is_request_permissions_tool(pending.name())
        && registered_tool.action_kind() == crate::ToolActionKind::RuntimeControl
    {
        return execute_permission_request_tool_call(inner, &pending, context, false).await;
    }

    let fully_trusted = inner.permission_review_mode.is_fully_trusted();
    let mut policy_decision = DefaultActionPolicy.decide(registered_tool.action_kind());
    if fully_trusted {
        policy_decision =
            ActionPolicyDecision::allow_fully_trusted_action(registered_tool.action_kind());
    }
    let mut allowed_proposal = None;
    if !policy_decision.is_allowed()
        || (fully_trusted
            && registered_tool.action_kind().is_mutating()
            && registered_tool.action_kind() != crate::ToolActionKind::Network
            && registered_tool.proposals_enabled())
    {
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
                    let events = {
                        let mut session = inner.session.lock().await;
                        if context.cancellation_token().is_cancelled() {
                            return Err(RuntimeError::ToolExecutionCancelled {
                                session_id: inner.session_id.clone(),
                                call_id: call_id.clone(),
                            });
                        }
                        session.submit_tool_execution_outcome(
                            call_id, status, content, diagnostic, None,
                        )?
                    };
                    return Ok(events);
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
            if let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence()
                && process_call_requests_permission(&pending)
            {
                return execute_process_permission_request_tool_call(
                    inner, &pending, intent, context, false,
                )
                .await;
            }
            let host_process_path_review = inner
                .accepted_local_workspace_process_runner
                .as_ref()
                .is_some_and(|accepted| {
                    accepted.admission.sandbox_profile()
                        == crate::LocalWorkspaceProcessSandboxProfile::Host
                        && matches!(
                            proposal.evidence(),
                            ActionProposalEvidence::ProcessAction(intent)
                                if crate::process::requires_host_process_path_review(intent)
                        )
                });
            if inner.allow_low_risk_apply_patches
                && pending.name().as_str() == APPLY_PATCH_TOOL_NAME
                && is_low_risk_apply_patch_proposal(registered_tool.action_kind(), &proposal)
            {
                policy_decision = ActionPolicyDecision::allow_low_risk_apply_patch();
                allowed_proposal = Some(proposal);
            } else if let Some(runner) = inner.low_risk_process_runner.clone()
                && !host_process_path_review
                && is_low_risk_process_action_proposal(registered_tool.action_kind(), &proposal)
            {
                policy_decision = ActionPolicyDecision::allow_low_risk_process_action();
                return execute_admitted_process_action(
                    inner,
                    &pending,
                    proposal,
                    ProcessExecutionAdmission::new(
                        policy_decision,
                        ProcessPermissionProfileId::READ_ONLY,
                        runner,
                        false,
                    ),
                    context,
                )
                .await;
            } else if let Some(runner) = inner.read_only_shell_process_runner.clone()
                && !host_process_path_review
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
                    ProcessExecutionAdmission::new(
                        policy_decision,
                        ProcessPermissionProfileId::SHELL_READ_ONLY,
                        runner,
                        false,
                    ),
                    context,
                )
                .await;
            } else if let Some(accepted) = inner.accepted_local_workspace_process_runner.clone()
                && !host_process_path_review
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
                    ProcessExecutionAdmission::new(
                        policy_decision,
                        accepted.admission.permission_profile_id(),
                        accepted.runner,
                        false,
                    ),
                    context,
                )
                .await;
            } else if let Some(accepted) = inner.accepted_local_workspace_process_runner.clone()
                && registered_tool.action_kind() == crate::ToolActionKind::CommandExec
                && proposal.action_kind() == crate::ToolActionKind::CommandExec
                && let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence()
                && accepted.admission.matches_intent(intent)
            {
                // The classifier is an audit/risk signal. The configured
                // runner boundary, rather than a command allowlist, decides
                // which capabilities the process actually receives.
                let risk_tier =
                    classify_tool_action_risk(registered_tool.action_kind(), Some(&proposal));
                policy_decision = if fully_trusted {
                    ActionPolicyDecision::allow_fully_trusted_action(registered_tool.action_kind())
                        .with_risk_tier(risk_tier)
                } else {
                    ActionPolicyDecision::allow_configured_process_action()
                        .with_risk_tier(risk_tier)
                };
                let permission_profile_id = accepted.admission.permission_profile_id();
                let mut admission = ProcessExecutionAdmission::new(
                    policy_decision,
                    permission_profile_id,
                    accepted.runner,
                    false,
                );
                let requires_high_risk_review =
                    crate::process::requires_process_action_review(intent);
                let requires_host_path_review = accepted.admission.sandbox_profile()
                    == crate::LocalWorkspaceProcessSandboxProfile::Host
                    && crate::process::requires_host_process_path_review(intent);
                if !fully_trusted && (requires_high_risk_review || requires_host_path_review) {
                    let reason = if requires_high_risk_review {
                        "action review triggered: command uses a reviewed shell, privilege, recursive-delete, or git-state operation"
                    } else {
                        "action review triggered: host-mode command references a path outside the workspace or writes git metadata"
                    };
                    match review_process_action(inner, &pending, &proposal, &context, reason)
                        .await?
                    {
                        HighRiskActionReview::Approved(review) => {
                            admission = admission.with_permission_review(review);
                        }
                        HighRiskActionReview::Resolved(events) => return Ok(events),
                    }
                }
                return execute_admitted_process_action(
                    inner, &pending, proposal, admission, context,
                )
                .await;
            } else if fully_trusted {
                policy_decision =
                    ActionPolicyDecision::allow_fully_trusted_action(registered_tool.action_kind())
                        .with_risk_tier(classify_tool_action_risk(
                            registered_tool.action_kind(),
                            Some(&proposal),
                        ));
                allowed_proposal = Some(proposal);
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
                let events = {
                    let mut session = inner.session.lock().await;
                    if context.cancellation_token().is_cancelled() {
                        return Err(RuntimeError::ToolExecutionCancelled {
                            session_id: inner.session_id.clone(),
                            call_id: call_id.clone(),
                        });
                    }
                    let result = ToolCallResult::failed(pending.id().clone(), diagnostic);
                    session.submit_tool_result(result, content)?
                };
                trace_denied_tool_execution(inner.session_id.as_str(), &pending, &events);
                return Ok(events);
            }
        } else if fully_trusted {
            // Explicit trusted mode authorizes configured mutating tools even
            // when their executor does not expose proposal evidence.
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
            let events = {
                let mut session = inner.session.lock().await;
                if context.cancellation_token().is_cancelled() {
                    return Err(RuntimeError::ToolExecutionCancelled {
                        session_id: inner.session_id.clone(),
                        call_id: call_id.clone(),
                    });
                }
                let result = ToolCallResult::failed(pending.id().clone(), diagnostic);
                session.submit_tool_result(result, content)?
            };
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
        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }
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
    let events = if let Some(proposal) = allowed_proposal {
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
        let mut session = inner.session.lock().await;
        let result = ToolCallResult::new(proposal.tool_call_id().clone(), status, diagnostic)?;
        session.submit_tool_result(result, content)?
    } else {
        let mut session = inner.session.lock().await;
        if context.cancellation_token().is_cancelled() && !preserves_control_state {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }
        session.submit_tool_execution_outcome(
            call_id,
            status,
            content,
            diagnostic,
            execution_evidence,
        )?
    };
    Ok(events)
}

async fn resolve_tool_admission_denial(
    inner: &Arc<RuntimeInner>,
    pending: &PendingToolCall,
    context: ToolExecutionContext,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    if context.cancellation_token().is_cancelled() {
        return Err(RuntimeError::ToolExecutionCancelled {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
        });
    }

    let diagnostic = diagnostic_from_text(
        DIAGNOSTIC_TOOL_NOT_ADMITTED,
        format!(
            "tool {} is not admitted for this runtime scope",
            pending.name()
        ),
    );
    let content = ToolOutput::json(
        serde_json::json!({
            "error": DIAGNOSTIC_TOOL_NOT_ADMITTED,
            "tool": pending.name().as_str(),
            "message": diagnostic.message(),
        })
        .to_string(),
    );
    let events = {
        let mut session = inner.session.lock().await;
        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: pending.id().clone(),
            });
        }
        session.submit_tool_execution_outcome(
            pending.id(),
            ToolCallResultStatus::Failed,
            content,
            Some(diagnostic),
            None,
        )?
    };
    Ok(events)
}

async fn resolve_plan_mode_read_only_denial(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    let message = "Rejected: workspace file edits are blocked while Plan Mode is active. Use `write_plan` to author or revise your Markdown plan, or call `exit_plan_mode` when ready for user approval.";
    let diagnostic = diagnostic_from_text("plan_mode_read_only", message);
    let content = ToolOutput::json(
        serde_json::json!({
            "ok": false,
            "tool": pending.name().as_str(),
            "error": {
                "code": "plan_mode_read_only",
                "message": message,
            }
        })
        .to_string(),
    );
    let events = {
        let mut session = inner.session.lock().await;
        session.submit_tool_execution_outcome(
            pending.id(),
            ToolCallResultStatus::Failed,
            content,
            Some(diagnostic),
            None,
        )?
    };
    Ok(events)
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
    _events: &[RuntimeJournalEvent],
) {
    tracing::info!(
        event = "runtime.tool.execute.finish",
        session_id,
        tool_call_id = pending.id().as_str(),
        tool_name = pending.name().as_str(),
        status = "denied",
        diagnostic_code = DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
        "runtime tool execution denied"
    );
}

pub(super) fn trace_review_denied_tool_execution(
    session_id: &str,
    pending: &PendingToolCall,
    _events: &[RuntimeJournalEvent],
    status: &'static str,
) {
    tracing::info!(
        event = "runtime.tool.execute.finish",
        session_id,
        tool_call_id = pending.id().as_str(),
        tool_name = pending.name().as_str(),
        status,
        diagnostic_code = if status == "review_failed" {
            "permission_review_failed"
        } else {
            "permission_request_denied"
        },
        "runtime tool execution stopped by high-risk action review"
    );
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
            context.with_approved_apply_patch(patch.clone())
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

    if decision.is_fully_trusted() {
        return Ok(());
    }

    let low_risk_apply_patch_admitted = decision.is_allowed()
        && decision.action_kind() == crate::ToolActionKind::WorkspaceWrite
        && decision.risk_tier() == crate::action_policy::ActionRiskTier::EditLow
        && pending.name().as_str() == APPLY_PATCH_TOOL_NAME
        && action_kind == crate::ToolActionKind::WorkspaceWrite
        && proposal.is_some_and(|proposal| is_low_risk_apply_patch_proposal(action_kind, proposal));

    if !low_risk_apply_patch_admitted {
        return Err(RuntimeError::MutatingActionCommitLifecycleRequired {
            session_id: session_id.clone(),
            call_id: pending.id().clone(),
            action_kind,
        });
    }

    Ok(())
}
