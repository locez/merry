//! PyO3 adapter for the owned run and its tool handoff lifecycle.

use crate::{
    error, protocol,
    run_state::{RunState, restore_run, take_run, take_run_for_cancel},
};
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Single-consumer owned run handle.
#[pyclass(name = "AgentRun")]
pub(crate) struct PyAgentRun {
    inner: Arc<RunState>,
    cancellation: CancellationToken,
}

impl PyAgentRun {
    pub(crate) fn new(run: merry::binding::OwnedAgentRun) -> Self {
        Self {
            inner: Arc::new(RunState::new(run)),
            cancellation: CancellationToken::new(),
        }
    }
}

struct RunLease {
    state: Arc<RunState>,
    run: Option<merry::binding::OwnedAgentRun>,
}

impl RunLease {
    fn take(state: Arc<RunState>) -> Result<Self, String> {
        let run = take_run(&state)?;
        Ok(Self {
            state,
            run: Some(run),
        })
    }

    async fn take_for_cancel(state: Arc<RunState>) -> Result<Self, String> {
        let run = take_run_for_cancel(&state).await?;
        Ok(Self {
            state,
            run: Some(run),
        })
    }

    fn run_mut(&mut self) -> Result<&mut merry::binding::OwnedAgentRun, String> {
        self.run
            .as_mut()
            .ok_or_else(|| "agent run lease has already been restored".to_owned())
    }

    fn restore(mut self) -> Result<(), String> {
        let Some(run) = self.run.take() else {
            return Ok(());
        };
        restore_run(&self.state, run)
    }
}

impl Drop for RunLease {
    fn drop(&mut self) {
        let Some(run) = self.run.take() else {
            return;
        };
        let _ = restore_run(&self.state, run);
    }
}

#[pymethods]
impl PyAgentRun {
    fn next<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.inner);
        let cancellation = self.cancellation.clone();
        let lease = RunLease::take(state).map_err(error::run_state_message_to_py)?;
        future_into_py(py, async move {
            let mut lease = lease;
            let outcome = {
                let run = lease.run_mut().map_err(error::run_state_message_to_py)?;
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        run.cancel().await.map_err(error::agent_error_to_py)?;
                        Ok(None)
                    }
                    message = run.next() => {
                        message.map_err(error::agent_error_to_py)
                    },
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
        let state = Arc::clone(&self.inner);
        let cancellation = self.cancellation.clone();
        let lease = RunLease::take(state).map_err(error::run_state_message_to_py)?;
        future_into_py(py, async move {
            let mut lease = lease;
            let outcome = {
                let run = lease.run_mut().map_err(error::run_state_message_to_py)?;
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        run.cancel().await.map_err(error::agent_error_to_py)?;
                        Ok(None)
                    }
                    submission = run.submit_tool_invocation_results(&batch_id, results) => {
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

    fn result<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.inner);
        let cancellation = self.cancellation.clone();
        let lease = RunLease::take(state).map_err(error::run_state_message_to_py)?;
        future_into_py(py, async move {
            let mut lease = lease;
            let result = {
                let run = lease.run_mut().map_err(error::run_state_message_to_py)?;
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => run.cancel().await,
                    result = run.result() => result,
                }
            };
            lease.restore().map_err(error::run_state_message_to_py)?;
            let result = result.map_err(error::agent_error_to_py)?;
            protocol::run_result_to_json(&result).map_err(error::serialization_error_to_py)
        })
    }

    fn cancel<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.cancellation.cancel();
        let state = Arc::clone(&self.inner);
        future_into_py(py, async move {
            let mut lease = RunLease::take_for_cancel(state)
                .await
                .map_err(error::run_state_message_to_py)?;
            let result = lease
                .run_mut()
                .map_err(error::run_state_message_to_py)?
                .cancel()
                .await
                .map_err(error::agent_error_to_py)?;
            lease.restore().map_err(error::run_state_message_to_py)?;
            protocol::run_result_to_json(&result).map_err(error::serialization_error_to_py)
        })
    }
}
