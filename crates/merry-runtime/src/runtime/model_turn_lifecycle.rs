use super::{
    RuntimeInner,
    journal_emission::{
        send_cancelled_event, send_failed_event, trace_provider_step_cancelled,
        trace_provider_step_failed,
    },
};
use crate::{
    events::RuntimeJournalEventBatch,
    session::{ModelTurnId, ModelTurnStatus},
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) async fn fail_model_turn(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    turn_id: ModelTurnId,
    diagnostic: merry_core::ErrorInfo,
) {
    {
        let mut session = inner.session.lock().await;
        let result = if session.model_turn_status(turn_id) == Some(ModelTurnStatus::InProgress) {
            session.abort_model_turn(turn_id)
        } else {
            Ok(())
        };
        if let Err(error) = result {
            tracing::error!(
                category = "model_turn_abort",
                model_turn_id = turn_id.as_u64(),
                error = %error,
                "failed to abort model turn before provider failure event"
            );
        }
    }
    trace_provider_step_failed(&diagnostic);
    let _ = send_failed_event(inner, sender, token, diagnostic).await;
}

pub(super) async fn cancel_model_turn(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    turn_id: ModelTurnId,
) {
    {
        let mut session = inner.session.lock().await;
        let result = if session.model_turn_status(turn_id) == Some(ModelTurnStatus::InProgress) {
            session.abort_model_turn(turn_id)
        } else {
            Ok(())
        };
        if let Err(error) = result {
            tracing::error!(
                category = "model_turn_abort",
                model_turn_id = turn_id.as_u64(),
                error = %error,
                "failed to abort model turn before provider cancellation event"
            );
        }
    }
    trace_provider_step_cancelled();
    let _ = send_cancelled_event(inner, sender).await;
}

/// Ensures task abortion cannot strand a turn before async cancellation code runs.
pub(super) struct InProgressModelTurnGuard {
    inner: Arc<RuntimeInner>,
    turn_id: ModelTurnId,
}

impl InProgressModelTurnGuard {
    pub(super) fn new(inner: Arc<RuntimeInner>, turn_id: ModelTurnId) -> Self {
        Self { inner, turn_id }
    }
}

impl Drop for InProgressModelTurnGuard {
    fn drop(&mut self) {
        if abort_in_progress_turn_if_unlocked(&self.inner, self.turn_id) {
            return;
        }

        let inner = Arc::clone(&self.inner);
        let turn_id = self.turn_id;
        tokio::spawn(async move {
            let mut session = inner.session.lock().await;
            abort_in_progress_turn(&mut session, turn_id);
        });
    }
}

fn abort_in_progress_turn_if_unlocked(inner: &RuntimeInner, turn_id: ModelTurnId) -> bool {
    let Ok(mut session) = inner.session.try_lock() else {
        return false;
    };
    abort_in_progress_turn(&mut session, turn_id);
    true
}

fn abort_in_progress_turn(session: &mut crate::session::SessionState, turn_id: ModelTurnId) {
    if session.model_turn_status(turn_id) != Some(ModelTurnStatus::InProgress) {
        return;
    }
    if let Err(error) = session.abort_model_turn(turn_id) {
        tracing::error!(
            category = "model_turn_abort",
            model_turn_id = turn_id.as_u64(),
            error = %error,
            "failed to abort in-progress model turn while dropping provider step"
        );
    }
}
