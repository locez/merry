//! PyO3 adapter for the owned interactive run handle.

use crate::{error, protocol};
use merry::{
    AgentError, FileSessionStore, InteractiveControl, InteractiveInput, InterruptReason,
    QueuedInputId, binding::OwnedInteractiveRun, binding::OwnedInteractiveStream,
};
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct InteractiveStreamState {
    stream: Mutex<Option<OwnedInteractiveStream>>,
    changed: Notify,
}

impl InteractiveStreamState {
    fn new(stream: OwnedInteractiveStream) -> Self {
        Self {
            stream: Mutex::new(Some(stream)),
            changed: Notify::new(),
        }
    }
}

struct InteractiveStreamLease {
    state: Arc<InteractiveStreamState>,
    stream: Option<OwnedInteractiveStream>,
}

impl InteractiveStreamLease {
    fn take(state: Arc<InteractiveStreamState>) -> Result<Self, String> {
        let stream = state
            .stream
            .lock()
            .map_err(|_| "interactive stream state is poisoned".to_owned())?
            .take()
            .ok_or_else(|| "interactive stream operation is already in progress".to_owned())?;
        Ok(Self {
            state,
            stream: Some(stream),
        })
    }

    async fn take_for_close(state: Arc<InteractiveStreamState>) -> Result<Self, String> {
        loop {
            let notified = state.changed.notified();
            let taken = state
                .stream
                .lock()
                .map_err(|_| "interactive stream state is poisoned".to_owned())?
                .take();
            if let Some(stream) = taken {
                drop(notified);
                return Ok(Self {
                    state,
                    stream: Some(stream),
                });
            }
            notified.await;
        }
    }

    fn stream_mut(&mut self) -> Result<&mut OwnedInteractiveStream, String> {
        self.stream
            .as_mut()
            .ok_or_else(|| "interactive stream lease has already been restored".to_owned())
    }

    fn restore(mut self) -> Result<(), String> {
        let Some(stream) = self.stream.take() else {
            return Ok(());
        };
        restore_stream(&self.state, stream)
    }
}

impl Drop for InteractiveStreamLease {
    fn drop(&mut self) {
        let Some(stream) = self.stream.take() else {
            return;
        };
        let _ = restore_stream(&self.state, stream);
    }
}

fn restore_stream(
    state: &InteractiveStreamState,
    stream: OwnedInteractiveStream,
) -> Result<(), String> {
    let mut guard = state
        .stream
        .lock()
        .map_err(|_| "interactive stream state is poisoned".to_owned())?;
    if guard.is_some() {
        return Err("interactive stream state was restored more than once".to_owned());
    }
    *guard = Some(stream);
    state.changed.notify_waiters();
    Ok(())
}

/// Long-lived interactive run bridging Rust stream, input queue, and control handles.
#[pyclass(name = "InteractiveRun")]
pub(crate) struct PyInteractiveRun {
    stream_state: Arc<InteractiveStreamState>,
    input: InteractiveInput,
    control: InteractiveControl,
    session_store_path: Option<String>,
    cancellation: CancellationToken,
}

impl PyInteractiveRun {
    pub(crate) fn new(run: OwnedInteractiveRun, session_store_path: Option<String>) -> Self {
        let (stream, input, control) = run.split();
        Self {
            stream_state: Arc::new(InteractiveStreamState::new(stream)),
            input,
            control,
            session_store_path,
            cancellation: CancellationToken::new(),
        }
    }
}

#[pymethods]
impl PyInteractiveRun {
    fn next<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.stream_state);
        let cancellation = self.cancellation.clone();
        let lease = InteractiveStreamLease::take(state).map_err(error::run_state_message_to_py)?;
        future_into_py(py, async move {
            let mut lease = lease;
            let outcome = {
                let stream = lease.stream_mut().map_err(error::run_state_message_to_py)?;
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        stream.request_cancel();
                        Ok(None)
                    }
                    message = stream.next_message() => {
                        message.map_err(error::agent_error_to_py)
                    }
                }
            };
            lease.restore().map_err(error::run_state_message_to_py)?;
            let message = outcome?;
            message
                .as_ref()
                .map(protocol::message_to_json)
                .transpose()
                .map_err(error::serialization_error_to_py)
        })
    }

    fn submit_tool_results<'py>(
        &self,
        py: Python<'py>,
        batch_id: String,
        results_json: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let batch_id =
            protocol::parse_batch_id(&batch_id).map_err(error::protocol_message_to_py)?;
        let results =
            protocol::parse_tool_results(&results_json).map_err(error::protocol_message_to_py)?;
        let state = Arc::clone(&self.stream_state);
        let cancellation = self.cancellation.clone();
        let lease = InteractiveStreamLease::take(state).map_err(error::run_state_message_to_py)?;
        future_into_py(py, async move {
            let mut lease = lease;
            let outcome = {
                let stream = lease.stream_mut().map_err(error::run_state_message_to_py)?;
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        stream.request_cancel();
                        Ok(None)
                    }
                    submission = stream.submit_tool_invocation_results(&batch_id, results) => {
                        submission.map(Some).map_err(error::agent_error_to_py)
                    }
                }
            };
            lease.restore().map_err(error::run_state_message_to_py)?;
            let Some(submission) = outcome? else {
                return Err(error::cancelled_operation_to_py());
            };
            protocol::submission_to_json(submission).map_err(error::serialization_error_to_py)
        })
    }

    fn submit<'py>(&self, py: Python<'py>, text: String) -> PyResult<Bound<'py, PyAny>> {
        let input = self.input.clone();
        future_into_py(py, async move {
            let item = input
                .submit(&text)
                .await
                .map_err(|err| error::agent_error_to_py(AgentError::from(err)))?;
            protocol::interactive_input_item_to_json(&item)
                .map_err(error::serialization_error_to_py)
        })
    }

    fn update_input<'py>(
        &self,
        py: Python<'py>,
        input_id: u64,
        text: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let input = self.input.clone();
        let id = QueuedInputId::from_u64(input_id);
        future_into_py(py, async move {
            input
                .update_by_id(id, &text)
                .await
                .map_err(|err| error::agent_error_to_py(AgentError::from(err)))
        })
    }

    fn remove_input<'py>(&self, py: Python<'py>, input_id: u64) -> PyResult<Bound<'py, PyAny>> {
        let input = self.input.clone();
        let id = QueuedInputId::from_u64(input_id);
        future_into_py(py, async move {
            input
                .remove_by_id(id)
                .await
                .map_err(|err| error::agent_error_to_py(AgentError::from(err)))
        })
    }

    fn set_paused<'py>(&self, py: Python<'py>, paused: bool) -> PyResult<Bound<'py, PyAny>> {
        let input = self.input.clone();
        future_into_py(py, async move {
            input
                .set_paused(paused)
                .await
                .map_err(|err| error::agent_error_to_py(AgentError::from(err)))
        })
    }

    fn flush<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let input = self.input.clone();
        future_into_py(py, async move {
            input
                .flush()
                .await
                .map_err(|err| error::agent_error_to_py(AgentError::from(err)))
        })
    }

    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let input = self.input.clone();
        future_into_py(py, async move {
            let snapshot = input
                .snapshot()
                .await
                .map_err(|err| error::agent_error_to_py(AgentError::from(err)))?;
            protocol::interactive_input_snapshot_to_json(&snapshot)
                .map_err(error::serialization_error_to_py)
        })
    }

    fn interrupt<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let control = self.control.clone();
        future_into_py(py, async move {
            control
                .interrupt(InterruptReason::User)
                .await
                .map_err(|err| error::agent_error_to_py(AgentError::from(err)))
        })
    }

    fn save_session<'py>(
        &self,
        py: Python<'py>,
        path: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let resolved_path = path
            .or_else(|| self.session_store_path.clone())
            .ok_or_else(|| {
                error::config_message_to_py(
                    "save_session requires a path or a previously configured session store",
                )
            })?;
        let control = self.control.clone();
        let store = FileSessionStore::new(resolved_path);
        future_into_py(py, async move {
            control
                .save_session_to(store)
                .await
                .map_err(|err| error::agent_error_to_py(AgentError::from(err)))
        })
    }

    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let control = self.control.clone();
        let state = Arc::clone(&self.stream_state);
        let cancellation = self.cancellation.clone();
        cancellation.cancel();
        future_into_py(py, async move {
            let mut lease = InteractiveStreamLease::take_for_close(state)
                .await
                .map_err(error::run_state_message_to_py)?;
            let stream = lease.stream_mut().map_err(error::run_state_message_to_py)?;
            // Run `control.close()` and `cancel_and_wait_until_closed()` concurrently:
            // an idle producer wakes on `InteractiveCommand::Close`, while a busy or
            // backpressured producer unblocks via cancellation + draining.
            let (_, _) = tokio::join!(
                async {
                    let _ = control.close().await;
                },
                async {
                    let _ = stream.cancel_and_wait_until_closed().await;
                }
            );
            lease.restore().map_err(error::run_state_message_to_py)?;
            Ok(())
        })
    }
}
