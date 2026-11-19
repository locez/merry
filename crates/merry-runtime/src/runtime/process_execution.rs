use super::{RuntimeInner, diagnostic_from_text};
use crate::{
    ActionExecutionEvidence, ActionProposal, ProcessActionIntent, ProcessExitStatus,
    ProcessPermissionProfileId, ProcessRunner, ProcessRunnerContext, ProcessRunnerError,
    ProcessRunnerOutput, RuntimeError, action_policy::ActionPolicyDecision,
    permission::PermissionAdmissionReview, process::shell_process_input,
    tool::ActionProposalEvidence, tool::ToolExecutionContext,
};
use merry_core::{
    PendingToolCall, RuntimeJournalEvent, SessionId, ToolCallResult, ToolCallResultStatus,
};
use std::sync::Arc;

mod output;

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
    let content = output::result_content(
        &intent,
        &output,
        permission_profile_id,
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
    let _ = (&policy_decision, &execution_evidence);
    let result = ToolCallResult::new(proposal.tool_call_id().clone(), status, diagnostic)?;
    let result_events = {
        let mut session = inner.session.lock().await;
        session.submit_tool_result(result, content)?
    };
    Ok(result_events)
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

fn process_status_label(status: ProcessExitStatus) -> String {
    match status {
        ProcessExitStatus::Exited(code) => format!("exit code {code}"),
        ProcessExitStatus::Cancelled => "cancelled".to_owned(),
        ProcessExitStatus::FailedToStart => "failed to start".to_owned(),
        ProcessExitStatus::DomainFailed => "domain failed".to_owned(),
        ProcessExitStatus::Running => "still running".to_owned(),
    }
}
