use super::{RuntimeInner, diagnostic_from_text, persist_resume_safe_savepoint_if_configured};
use crate::{
    ActionExecutionEvidence, ActionProposal, ArtifactContent, ProcessActionIntent,
    ProcessExitStatus, ProcessPermissionProfileId, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerOutput, RuntimeError, action_audit::ActionAuditPolicy,
    action_policy::ActionPolicyDecision, permission::PermissionAdmissionReview,
    process::ShellProcessInput, process::shell_process_input,
    session::ProposedToolExecutionOutcome, session::ToolResultLedgerObservation,
    tool::ActionProposalEvidence, tool::ToolExecutionContext,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use merry_core::{
    ArtifactKind, ArtifactRef, PendingToolCall, RuntimeJournalEvent, SessionId,
    ToolCallResultStatus,
};
use std::sync::Arc;

pub(super) struct ProcessExecutionAdmission {
    policy_decision: ActionPolicyDecision,
    permission_profile_id: ProcessPermissionProfileId,
    runner: Arc<dyn ProcessRunner>,
    attribute_plan_effect: bool,
    permission_review: Option<PermissionAdmissionReview>,
}

impl ProcessExecutionAdmission {
    pub(super) fn new(
        policy_decision: ActionPolicyDecision,
        permission_profile_id: ProcessPermissionProfileId,
        runner: Arc<dyn ProcessRunner>,
        attribute_plan_effect: bool,
    ) -> Self {
        Self {
            policy_decision,
            permission_profile_id,
            runner,
            attribute_plan_effect,
            permission_review: None,
        }
    }

    pub(super) fn with_permission_review(mut self, review: PermissionAdmissionReview) -> Self {
        self.permission_review = Some(review);
        self
    }
}

pub(super) async fn execute_admitted_process_action(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    proposal: ActionProposal,
    admission: ProcessExecutionAdmission,
    context: ToolExecutionContext,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    let ProcessExecutionAdmission {
        policy_decision,
        permission_profile_id,
        runner,
        attribute_plan_effect,
        permission_review,
    } = admission;
    let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence().clone() else {
        return Err(RuntimeError::ToolExecutionFailed {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
            message: "admitted process proposal did not include process action evidence".to_owned(),
        });
    };

    if context.cancellation_token().is_cancelled() {
        return Err(RuntimeError::ToolExecutionCancelled {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
        });
    }

    let shell_input_artifact = if let Some(shell_input) = shell_process_input(&intent) {
        let input_content =
            shell_input_artifact_content(shell_input, &intent, permission_profile_id, pending);
        let mut session = inner.session.lock().await;
        let recorded = session
            .record_process_input_artifact(input_content)
            .map_err(RuntimeError::from)?;
        Some(recorded)
    } else {
        None
    };

    if context.cancellation_token().is_cancelled() {
        return Err(RuntimeError::ToolExecutionCancelled {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
        });
    }

    if attribute_plan_effect {
        inner
            .record_plan_runtime_effect(Vec::new())
            .await
            .map_err(|error| RuntimeError::PlanEffectAttribution {
                session_id: inner.session_id.clone(),
                call_id: pending.id().clone(),
                message: error.to_string(),
            })?;
    }

    let runner_context = ProcessRunnerContext::new(context.cancellation_token().clone());
    trace_process_execution_start(&inner.session_id, pending, &intent, permission_profile_id);
    let output = tokio::select! {
        biased;
        () = context.cancellation_token().cancelled() => {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: pending.id().clone(),
            });
        }
        output = runner.run(intent.clone(), runner_context) => output,
    };

    let output = match output {
        Ok(output) => output,
        Err(ProcessRunnerError::Cancelled) => {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: inner.session_id.clone(),
                call_id: pending.id().clone(),
            });
        }
        Err(ProcessRunnerError::Infrastructure { message }) => {
            if context.cancellation_token().is_cancelled() {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: inner.session_id.clone(),
                    call_id: pending.id().clone(),
                });
            }
            return Err(RuntimeError::ToolExecutionFailed {
                session_id: inner.session_id.clone(),
                call_id: pending.id().clone(),
                message,
            });
        }
    };

    let execution_evidence = output
        .execution_evidence(&intent, permission_profile_id)
        .map(ActionExecutionEvidence::ProcessAction)
        .map_err(|source| RuntimeError::ToolExecutionFailed {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
            message: format!("process execution evidence did not match intent: {source}"),
        })?;
    trace_process_execution_finish(
        &inner.session_id,
        pending,
        &intent,
        permission_profile_id,
        &output,
    );
    let shell_input_artifact_ref = shell_input_artifact
        .as_ref()
        .map(|(artifact, _events)| artifact);
    let content = process_output_artifact_content(
        &intent,
        &output,
        permission_profile_id,
        shell_input_artifact_ref,
        permission_review.as_ref(),
    );
    let status = if output.ok() {
        ToolCallResultStatus::Succeeded
    } else {
        ToolCallResultStatus::Failed
    };
    let diagnostic = if output.ok() {
        None
    } else {
        Some(diagnostic_from_text(
            "process_action_failed",
            format!(
                "process action completed with status {}",
                process_status_label(output.status())
            ),
        ))
    };
    let observation = process_result_ledger_observation(
        &intent,
        &output,
        status,
        permission_profile_id,
        shell_input_artifact_ref,
    );

    let result_events = {
        let mut session = inner.session.lock().await;
        session.submit_proposed_tool_execution_outcome_record(
            ProposedToolExecutionOutcome::new(
                proposal,
                status,
                content,
                diagnostic,
                Some(execution_evidence),
                ActionAuditPolicy::from_decision(&policy_decision),
            )
            .with_observation(observation),
        )?
    };
    persist_resume_safe_savepoint_if_configured(inner).await;
    Ok(merge_process_input_and_result_events(
        shell_input_artifact.map(|(_artifact, events)| events),
        result_events,
    ))
}

fn trace_process_execution_start(
    session_id: &SessionId,
    pending: &PendingToolCall,
    intent: &ProcessActionIntent,
    permission_profile_id: ProcessPermissionProfileId,
) {
    if let Some(shell_input) = shell_process_input(intent) {
        let script_fingerprint = shell_input.script_fingerprint();
        tracing::info!(
            event = "runtime.process.execute.start",
            session_id = session_id.as_str(),
            tool_call_id = pending.id().as_str(),
            tool_name = pending.name().as_str(),
            permission_profile_id = permission_profile_id.as_str(),
            argv_count = intent.argv().len(),
            shell = shell_input.shell(),
            shell_flag = shell_input.flag(),
            shell_script_bytes = shell_input.script_bytes(),
            shell_script_fingerprint = script_fingerprint.as_str(),
            cwd = intent.cwd().unwrap_or("."),
            stdout_limit_bytes = intent.stdout_limit_bytes(),
            stderr_limit_bytes = intent.stderr_limit_bytes(),
            "runtime process execution start"
        );
        return;
    }

    tracing::info!(
        event = "runtime.process.execute.start",
        session_id = session_id.as_str(),
        tool_call_id = pending.id().as_str(),
        tool_name = pending.name().as_str(),
        permission_profile_id = permission_profile_id.as_str(),
        argv = ?intent.argv(),
        cwd = intent.cwd().unwrap_or("."),
        stdout_limit_bytes = intent.stdout_limit_bytes(),
        stderr_limit_bytes = intent.stderr_limit_bytes(),
        "runtime process execution start"
    );
}

fn trace_process_execution_finish(
    session_id: &SessionId,
    pending: &PendingToolCall,
    intent: &ProcessActionIntent,
    permission_profile_id: ProcessPermissionProfileId,
    output: &ProcessRunnerOutput,
) {
    if let Some(shell_input) = shell_process_input(intent) {
        let script_fingerprint = shell_input.script_fingerprint();
        tracing::info!(
            event = "runtime.process.execute.finish",
            session_id = session_id.as_str(),
            tool_call_id = pending.id().as_str(),
            tool_name = pending.name().as_str(),
            permission_profile_id = permission_profile_id.as_str(),
            shell = shell_input.shell(),
            shell_flag = shell_input.flag(),
            shell_script_bytes = shell_input.script_bytes(),
            shell_script_fingerprint = script_fingerprint.as_str(),
            status = %process_status_label(output.status()),
            stdout_bytes = output.stdout_bytes(),
            stderr_bytes = output.stderr_bytes(),
            stdout_truncated = output.stdout_truncated(),
            stderr_truncated = output.stderr_truncated(),
            "runtime process execution finish"
        );
        return;
    }

    tracing::info!(
        event = "runtime.process.execute.finish",
        session_id = session_id.as_str(),
        tool_call_id = pending.id().as_str(),
        tool_name = pending.name().as_str(),
        permission_profile_id = permission_profile_id.as_str(),
        status = %process_status_label(output.status()),
        stdout_bytes = output.stdout_bytes(),
        stderr_bytes = output.stderr_bytes(),
        stdout_truncated = output.stdout_truncated(),
        stderr_truncated = output.stderr_truncated(),
        "runtime process execution finish"
    );
}

fn shell_input_artifact_content(
    shell_input: ShellProcessInput<'_>,
    intent: &ProcessActionIntent,
    permission_profile_id: ProcessPermissionProfileId,
    pending: &PendingToolCall,
) -> ArtifactContent {
    ArtifactContent::json(
        serde_json::json!({
            "kind": "shell_command_input",
            "permission_profile_id": permission_profile_id.as_str(),
            "tool_call_id": pending.id().as_str(),
            "tool_name": pending.name().as_str(),
            "intent": {
                "summary": intent.summary(),
                "cwd": intent.cwd(),
            },
            "input_evidence": {
                "kind": "shell_command_script",
                "shell": shell_input.shell(),
                "flag": shell_input.flag(),
                "script": shell_input.script(),
                "script_bytes": shell_input.script_bytes(),
                "script_fingerprint": shell_input.script_fingerprint(),
            },
        })
        .to_string(),
    )
}

fn process_output_artifact_content(
    intent: &ProcessActionIntent,
    output: &ProcessRunnerOutput,
    permission_profile_id: ProcessPermissionProfileId,
    input_artifact: Option<&ArtifactRef>,
    permission_review: Option<&PermissionAdmissionReview>,
) -> ArtifactContent {
    let shell_input = shell_process_input(intent);
    let intent_payload = if let Some(shell_input) = shell_input {
        serde_json::json!({
            "summary": intent.summary(),
            "command": shell_input.script(),
            "cwd": intent.cwd(),
        })
    } else {
        serde_json::json!({
            "summary": intent.summary(),
            "argv": intent.argv(),
            "cwd": intent.cwd(),
        })
    };

    let mut stdout_payload = serde_json::json!({
        "text": output.stdout_text(),
        "bytes": output.stdout_bytes(),
        "truncated": output.stdout_truncated(),
        "utf8": output.stdout_is_utf8(),
    });
    if !output.stdout_is_utf8() {
        stdout_payload["bytes_base64"] = serde_json::json!(BASE64.encode(output.stdout_data()));
    }

    let mut stderr_payload = serde_json::json!({
        "text": output.stderr_text(),
        "bytes": output.stderr_bytes(),
        "truncated": output.stderr_truncated(),
        "utf8": output.stderr_is_utf8(),
    });
    if !output.stderr_is_utf8() {
        stderr_payload["bytes_base64"] = serde_json::json!(BASE64.encode(output.stderr_data()));
    }

    let mut payload = serde_json::json!({
        "ok": output.ok(),
        "kind": "process_action",
        "permission_profile_id": permission_profile_id.as_str(),
        "status": process_status_json(output.status()),
        "intent": intent_payload,
        "stdout": stdout_payload,
        "stderr": stderr_payload,
    });

    if let Some(review) = permission_review {
        payload["permission_review"] = serde_json::json!({
            "source": review.source().as_str(),
            "risk": review.risk().as_str(),
            "user_authorization": review.user_authorization().as_str(),
            "rationale": review.rationale(),
        });
    }

    if !output.ok() {
        payload["guidance"] = serde_json::json!({
            "kind": "process_action_recovery",
            "message": "The process action ran inside the sandbox and failed. If the failure is caused by unavailable network, filesystem path, or host integration access (including its required environment), call request_permissions for the exact same action before retrying it. Request every capability that command needs together, such as network plus dbus or ssh-agent; an unmodeled Linux Unix socket may be requested as its exact filesystem path.",
        });
    }

    if output.stdout_truncated() || output.stderr_truncated() {
        let truncated_guidance = serde_json::json!({
            "kind": "process_output_truncated",
            "message": "The captured process output was truncated. Do not assume omitted output is absent; rerun with a narrower command, filter, range, or targeted file inspection before drawing conclusions from the output.",
            "stdout_truncated": output.stdout_truncated(),
            "stderr_truncated": output.stderr_truncated(),
        });
        if output.ok() {
            payload["guidance"] = truncated_guidance;
        } else {
            payload["output_guidance"] = truncated_guidance;
        }
    }

    if let Some(input_artifact) = input_artifact {
        payload["input_artifact"] = artifact_ref_json(input_artifact);
    } else if let Some(shell_input) = shell_input {
        payload["input_evidence"] = serde_json::json!({
            "kind": "shell_command_script",
            "shell": shell_input.shell(),
            "flag": shell_input.flag(),
            "script": shell_input.script(),
            "script_bytes": shell_input.script_bytes(),
            "script_fingerprint": shell_input.script_fingerprint(),
        });
    }

    ArtifactContent::json(payload.to_string())
}

fn process_result_ledger_observation(
    intent: &ProcessActionIntent,
    output: &ProcessRunnerOutput,
    result_status: ToolCallResultStatus,
    permission_profile_id: ProcessPermissionProfileId,
    input_artifact: Option<&ArtifactRef>,
) -> ToolResultLedgerObservation {
    let mut summary = if let Some(shell_input) = shell_process_input(intent) {
        format!(
            "shell process action {}; permission_profile={}; result={}; shell={}; shell_flag={}; shell_script_bytes={}; shell_script_fingerprint={}; stdout_bytes={}; stderr_bytes={}",
            process_status_label(output.status()),
            permission_profile_id.as_str(),
            process_result_status_label(result_status),
            shell_input.shell(),
            shell_input.flag(),
            shell_input.script_bytes(),
            shell_input.script_fingerprint(),
            output.stdout_bytes(),
            output.stderr_bytes(),
        )
    } else {
        format!(
            "process action `{}` {}; permission_profile={}; result={}; stdout_bytes={}; stderr_bytes={}",
            intent.argv().join(" "),
            process_status_label(output.status()),
            permission_profile_id.as_str(),
            process_result_status_label(result_status),
            output.stdout_bytes(),
            output.stderr_bytes(),
        )
    };

    if output.stdout_truncated() {
        summary.push_str("; stdout_truncated=true");
    }
    if output.stderr_truncated() {
        summary.push_str("; stderr_truncated=true");
    }
    if let Some(input_artifact) = input_artifact {
        summary.push_str("; input_artifact=");
        summary.push_str(input_artifact.id().as_str());
    }

    ToolResultLedgerObservation::new(crate::ledger::LedgerScope::Tool, summary)
        .expect("process result ledger summary is built from a non-empty static prefix")
}

fn artifact_ref_json(artifact: &ArtifactRef) -> serde_json::Value {
    serde_json::json!({
        "id": artifact.id().as_str(),
        "kind": artifact_kind_label(artifact.kind()),
    })
}

fn artifact_kind_label(kind: &ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Text => "text",
        ArtifactKind::Json => "json",
        ArtifactKind::Binary => "binary",
        ArtifactKind::Image => "image",
        ArtifactKind::Other => "other",
    }
}

fn merge_process_input_and_result_events(
    input_events: Option<Vec<RuntimeJournalEvent>>,
    result_events: Vec<RuntimeJournalEvent>,
) -> Vec<RuntimeJournalEvent> {
    let Some(mut input_events) = input_events else {
        return result_events;
    };

    input_events.extend(result_events);
    input_events
}

fn process_result_status_label(status: ToolCallResultStatus) -> &'static str {
    match status {
        ToolCallResultStatus::Succeeded => "succeeded",
        ToolCallResultStatus::Failed => "failed",
    }
}

fn process_status_json(status: ProcessExitStatus) -> serde_json::Value {
    match status {
        ProcessExitStatus::Exited(code) => {
            serde_json::json!({ "kind": "exited", "code": code })
        }
        ProcessExitStatus::Cancelled => serde_json::json!({ "kind": "cancelled" }),
        ProcessExitStatus::FailedToStart => serde_json::json!({ "kind": "failed_to_start" }),
        ProcessExitStatus::DomainFailed => serde_json::json!({ "kind": "domain_failed" }),
    }
}

fn process_status_label(status: ProcessExitStatus) -> String {
    match status {
        ProcessExitStatus::Exited(code) => format!("exit code {code}"),
        ProcessExitStatus::Cancelled => "cancelled".to_owned(),
        ProcessExitStatus::FailedToStart => "failed to start".to_owned(),
        ProcessExitStatus::DomainFailed => "domain failed".to_owned(),
    }
}
