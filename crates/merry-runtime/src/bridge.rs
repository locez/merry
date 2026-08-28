//! Runtime-owned host tool handoff primitives.
//!
//! Interactive and non-interactive loops share the same result command and
//! validation rules. Keeping this contract here prevents each surface from
//! growing a separate batch state machine.

use crate::{Runtime, RuntimeError, ToolExecutionOutcome, events::ActiveStepPermit};
use merry_core::{
    PendingToolCall, PendingToolCallBatch, RuntimeJournalEvent, ToolCallBatchId, ToolCallId,
};
use std::collections::BTreeSet;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub(crate) struct BridgeToolResultCommand {
    pub(crate) payload: BridgeToolResultPayload,
    pub(crate) ack_sender: oneshot::Sender<Result<(), RuntimeError>>,
}

impl BridgeToolResultCommand {
    pub(crate) fn outcomes(
        batch_id: ToolCallBatchId,
        outcomes: Vec<(ToolCallId, ToolExecutionOutcome)>,
        ack_sender: oneshot::Sender<Result<(), RuntimeError>>,
    ) -> Self {
        Self {
            payload: BridgeToolResultPayload::Outcomes { batch_id, outcomes },
            ack_sender,
        }
    }
}

pub(crate) enum BridgeToolResultPayload {
    Outcomes {
        batch_id: ToolCallBatchId,
        outcomes: Vec<(ToolCallId, ToolExecutionOutcome)>,
    },
}

pub(crate) async fn receive_bridge_tool_result(
    receiver: &mut mpsc::Receiver<BridgeToolResultCommand>,
    token: &CancellationToken,
) -> Option<BridgeToolResultCommand> {
    tokio::select! {
        command = receiver.recv() => command,
        () = token.cancelled() => None,
    }
}

pub(crate) async fn resolve_bridge_tool_result_command(
    runtime: &Runtime,
    expected_batch: &PendingToolCallBatch,
    command: BridgeToolResultCommand,
    loop_permit: &ActiveStepPermit,
) -> (
    oneshot::Sender<Result<(), RuntimeError>>,
    Result<Vec<RuntimeJournalEvent>, RuntimeError>,
) {
    let BridgeToolResultCommand {
        payload,
        ack_sender,
    } = command;
    let result = match payload {
        BridgeToolResultPayload::Outcomes { batch_id, outcomes } => {
            resolve_bridge_tool_outcomes(runtime, expected_batch, batch_id, outcomes, loop_permit)
                .await
        }
    };
    (ack_sender, result)
}

async fn resolve_bridge_tool_outcomes(
    runtime: &Runtime,
    expected_batch: &PendingToolCallBatch,
    received_batch_id: ToolCallBatchId,
    outcomes: Vec<(ToolCallId, ToolExecutionOutcome)>,
    loop_permit: &ActiveStepPermit,
) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
    if expected_batch.id() != &received_batch_id {
        return Err(RuntimeError::BridgeToolResultBatchIdMismatch {
            session_id: runtime.session_id().clone(),
            expected_batch_id: expected_batch.id().clone(),
            received_batch_id,
        });
    }
    let expected_calls = expected_batch.calls();
    let expected_call_ids = expected_calls
        .iter()
        .map(|call| call.id().clone())
        .collect::<Vec<_>>();
    let received_call_ids = outcomes
        .iter()
        .map(|(call_id, _)| call_id.clone())
        .collect::<Vec<_>>();
    let expected_set = expected_call_ids.iter().cloned().collect::<BTreeSet<_>>();
    let received_set = received_call_ids.iter().cloned().collect::<BTreeSet<_>>();
    if expected_call_ids.len() != received_call_ids.len() || expected_set != received_set {
        return Err(bridge_tool_result_batch_mismatch(
            runtime,
            expected_calls,
            received_call_ids,
        ));
    }

    runtime
        .submit_tool_execution_outcomes_with_active_permit(outcomes, loop_permit)
        .await
}

pub(crate) fn bridge_tool_result_batch_mismatch(
    runtime: &Runtime,
    expected_calls: &[PendingToolCall],
    received_call_ids: Vec<ToolCallId>,
) -> RuntimeError {
    RuntimeError::BridgeToolResultBatchMismatch {
        session_id: runtime.session_id().clone(),
        expected_call_ids: expected_calls
            .iter()
            .map(|call| call.id().clone())
            .collect(),
        received_call_ids,
    }
}
