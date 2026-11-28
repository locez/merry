//! Intrinsic execution for Plan Mode (`enter_plan_mode`, `write_plan`, `read_plan`,
//! `exit_plan_mode`) and checklist (`update_todos`) tools.

use crate::{
    RuntimeError, ToolOutput,
    plan::{
        EnterPlanModeInput, ExitPlanModeInput, PlanError, ReadPlanInput, WritePlanInput,
        unix_time_ms,
    },
    todo::{TodoError, UpdateTodosInput},
    tool::PlanIntrinsicTool,
};
use merry_core::{
    ErrorInfo, PendingToolCall, RuntimeJournalEvent, RuntimeJournalPayload, ToolCallResultStatus,
};
use serde::{Serialize, de::DeserializeOwned};

use super::RuntimeInner;

pub(super) async fn execute_plan_or_todo_tool_call(
    inner: &RuntimeInner,
    intrinsic: PlanIntrinsicTool,
    pending: &PendingToolCall,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    match intrinsic {
        PlanIntrinsicTool::EnterPlanMode => execute_enter_plan_mode(inner, pending).await,
        PlanIntrinsicTool::WritePlan => execute_write_plan(inner, pending).await,
        PlanIntrinsicTool::ReadPlan => execute_read_plan(inner, pending).await,
        PlanIntrinsicTool::ExitPlanMode => execute_exit_plan_mode(inner, pending).await,
        PlanIntrinsicTool::UpdateTodos => execute_update_todos(inner, pending).await,
    }
}

async fn execute_enter_plan_mode(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    let _input = match decode_input::<EnterPlanModeInput>(pending) {
        Ok(input) => input,
        Err(message) => {
            return submit_tool_failure(inner, pending, "plan_input_invalid", &message).await;
        }
    };

    let (mode_event, mode, snapshot) = {
        let mut session = inner.session.lock().await;
        let changed = session.plan_state_mut().activate_from_tool();
        let mode_event = changed
            .map(|mode| session.record_event(RuntimeJournalPayload::PlanModeChanged { mode }));
        let mode = session.plan_state().mode();
        let snapshot = session.plan_state().snapshot();
        (mode_event, mode, snapshot)
    };

    let output = serde_json::json!({
        "ok": true,
        "mode": mode,
        "active_plan_id": snapshot.active_plan_id,
        "existing_plans": snapshot.plans.len(),
        "message": "Entered read-only Plan Mode. Explore the repository without modifying workspace files, author your Markdown plan with write_plan, and call exit_plan_mode when ready for user review."
    });

    let mut events = Vec::new();
    if let Some(event) = mode_event {
        events.push(event);
    }
    submit_tool_success(inner, pending, output, events).await
}

async fn execute_write_plan(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    let input = match decode_input::<WritePlanInput>(pending) {
        Ok(input) => input,
        Err(message) => {
            return submit_tool_failure(inner, pending, "plan_input_invalid", &message).await;
        }
    };

    let now_ms = unix_time_ms();
    let (doc, saved_event) = {
        let mut session = inner.session.lock().await;
        let doc = match session.plan_state_mut().save_plan(
            &input.title,
            &input.content,
            input.plan_id.as_ref(),
            input.create_new,
            now_ms,
        ) {
            Ok(doc) => doc,
            Err(error) => {
                drop(session);
                return submit_tool_failure(
                    inner,
                    pending,
                    plan_error_code(&error),
                    &error.to_string(),
                )
                .await;
            }
        };
        let event = session.record_event(RuntimeJournalPayload::PlanSaved { plan: doc.clone() });
        (doc, event)
    };

    let mut file_path_display = doc.summary.file_name.clone();
    if let Some(store) = inner.persistence.store.as_ref() {
        let written_path = store
            .write_plan_file(&inner.session_id, &doc.summary.file_name, &doc.content)
            .await?;
        file_path_display = written_path.display().to_string();
    }

    let output = serde_json::json!({
        "ok": true,
        "plan_id": doc.summary.id,
        "title": doc.summary.title,
        "file_name": doc.summary.file_name,
        "file_path": file_path_display,
        "status": doc.summary.status,
        "revision": doc.summary.revision,
    });

    submit_tool_success(inner, pending, output, vec![saved_event]).await
}

async fn execute_read_plan(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    let input = match decode_input::<ReadPlanInput>(pending) {
        Ok(input) => input,
        Err(message) => {
            return submit_tool_failure(inner, pending, "plan_input_invalid", &message).await;
        }
    };

    let (snapshot, selected) = {
        let session = inner.session.lock().await;
        let snapshot = session.plan_state().snapshot();
        let selected = match input.plan_id.as_ref() {
            Some(plan_id) => match session.plan_state().document(plan_id) {
                Some(doc) => Some(doc),
                None => {
                    let message = PlanError::UnknownPlan {
                        plan_id: plan_id.clone(),
                    }
                    .to_string();
                    drop(session);
                    return submit_tool_failure(inner, pending, "plan_not_found", &message).await;
                }
            },
            None => session.plan_state().active_or_latest_document(),
        };
        (snapshot, selected)
    };

    let output = serde_json::json!({
        "ok": true,
        "mode": snapshot.mode,
        "active_plan_id": snapshot.active_plan_id,
        "awaiting_approval_plan_id": snapshot.awaiting_approval_plan_id,
        "plans": snapshot.plans,
        "selected": selected,
    });

    submit_tool_success(inner, pending, output, Vec::new()).await
}

async fn execute_exit_plan_mode(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    let input = match decode_input::<ExitPlanModeInput>(pending) {
        Ok(input) => input,
        Err(message) => {
            return submit_tool_failure(inner, pending, "plan_input_invalid", &message).await;
        }
    };

    let now_ms = unix_time_ms();
    let (doc, event) = {
        let mut session = inner.session.lock().await;
        let doc = match session
            .plan_state_mut()
            .request_approval(input.plan_id.as_ref(), now_ms)
        {
            Ok(doc) => doc,
            Err(error) => {
                drop(session);
                return submit_tool_failure(
                    inner,
                    pending,
                    plan_error_code(&error),
                    &error.to_string(),
                )
                .await;
            }
        };
        let event = session
            .record_event(RuntimeJournalPayload::PlanApprovalRequested { plan: doc.clone() });
        (doc, event)
    };

    if let Some(store) = inner.persistence.store.as_ref() {
        let _ = store
            .write_plan_file(&inner.session_id, &doc.summary.file_name, &doc.content)
            .await?;
    }

    let has_content = !doc.content.trim().is_empty();
    let message = if has_content {
        "Plan submitted for user review. Wait for the user to approve, request changes, or abandon the plan."
    } else {
        "Submitted an empty plan for user review. Wait for the user's decision."
    };

    let output = serde_json::json!({
        "ok": true,
        "plan_id": doc.summary.id,
        "title": doc.summary.title,
        "file_name": doc.summary.file_name,
        "revision": doc.summary.revision,
        "has_content": has_content,
        "message": message,
    });

    submit_tool_success(inner, pending, output, vec![event]).await
}

async fn execute_update_todos(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    let input = match decode_input::<UpdateTodosInput>(pending) {
        Ok(input) => input,
        Err(message) => {
            return submit_tool_failure(inner, pending, "todo_input_invalid", &message).await;
        }
    };

    let (summary, count, event) = {
        let mut session = inner.session.lock().await;
        let summary = match session.todo_list_mut().apply(input) {
            Ok(summary) => summary,
            Err(error) => {
                drop(session);
                return submit_tool_failure(
                    inner,
                    pending,
                    todo_error_code(&error),
                    &error.to_string(),
                )
                .await;
            }
        };
        let items = session.todo_list().items().to_vec();
        let count = items.len();
        let event = session.record_event(RuntimeJournalPayload::TodoListUpdated { items });
        (summary, count, event)
    };

    let output = serde_json::json!({
        "ok": true,
        "count": count,
        "summary": summary,
    });

    submit_tool_success(inner, pending, output, vec![event]).await
}

fn decode_input<T: DeserializeOwned>(pending: &PendingToolCall) -> Result<T, String> {
    pending
        .arguments()
        .deserialize_as()
        .map_err(|error| format!("could not decode {} input: {error}", pending.name()))
}

fn plan_error_code(error: &PlanError) -> &'static str {
    match error {
        PlanError::InvalidTitle { .. } => "plan_invalid_title",
        PlanError::ContentTooLarge { .. } => "plan_content_too_large",
        PlanError::EmptyFeedback => "plan_empty_feedback",
        PlanError::UnknownPlan { .. } => "plan_not_found",
        PlanError::PlanNotEditable { .. } => "plan_not_editable",
        PlanError::TooManyPlans { .. } => "plan_limit_exceeded",
        PlanError::InvalidFileName { .. } => "plan_invalid_file_name",
    }
}

fn todo_error_code(error: &TodoError) -> &'static str {
    match error {
        TodoError::DuplicateId { .. } => "todo_duplicate_id",
        TodoError::InvalidId { .. } => "todo_invalid_id",
        TodoError::ContentTooLong { .. } => "todo_content_too_long",
        TodoError::TooManyItems { .. } => "todo_limit_exceeded",
    }
}

async fn submit_tool_success<T: Serialize>(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    output: T,
    mut extra_events: Vec<RuntimeJournalEvent>,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    let content =
        serde_json::to_string(&output).map_err(|error| RuntimeError::ToolExecutionFailed {
            session_id: inner.session_id.clone(),
            call_id: pending.id().clone(),
            message: format!("failed to serialize {} output: {error}", pending.name()),
        })?;
    let events = {
        let mut session = inner.session.lock().await;
        session.submit_tool_execution_outcome(
            pending.id(),
            ToolCallResultStatus::Succeeded,
            ToolOutput::json(content),
            None,
            None,
        )?
    };
    extra_events.extend(events);
    Ok(extra_events)
}

async fn submit_tool_failure(
    inner: &RuntimeInner,
    pending: &PendingToolCall,
    code: &str,
    message: &str,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    let diagnostic = ErrorInfo::new(code, message).map_err(RuntimeError::from)?;
    let content = serde_json::json!({
        "ok": false,
        "tool": pending.name().as_str(),
        "error": {
            "code": code,
            "message": message,
        }
    });
    let events = {
        let mut session = inner.session.lock().await;
        session.submit_tool_execution_outcome(
            pending.id(),
            ToolCallResultStatus::Failed,
            ToolOutput::json(content.to_string()),
            Some(diagnostic),
            None,
        )?
    };
    Ok(events)
}
