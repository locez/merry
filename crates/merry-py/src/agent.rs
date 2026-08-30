//! PyO3 adapter for the built Rust agent.

use crate::{error, protocol, run::PyAgentRun};
use merry::Agent;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::sync::Arc;

/// Built Rust-owned agent handle.
#[pyclass(name = "Agent")]
pub(crate) struct PyAgent {
    inner: Arc<Agent>,
}

impl PyAgent {
    pub(crate) fn new(agent: Agent) -> Self {
        Self {
            inner: Arc::new(agent),
        }
    }
}

#[pymethods]
impl PyAgent {
    fn session_id(&self) -> String {
        self.inner.session_id().to_string()
    }

    fn stream(
        &self,
        task: String,
        final_output_schema_json: Option<String>,
    ) -> PyResult<PyAgentRun> {
        let agent = Arc::clone(&self.inner);
        let schema = final_output_schema_json
            .as_deref()
            .map(protocol::parse_input_schema)
            .transpose()
            .map_err(error::protocol_message_to_py)?;
        let result = pyo3_async_runtimes::tokio::get_runtime().block_on(async move {
            match schema {
                Some(schema) => agent.stream_with_owned_tool_handoff_and_schema(&task, schema),
                None => agent.stream_with_owned_tool_handoff(&task),
            }
        });
        result
            .map(PyAgentRun::new)
            .map_err(error::agent_error_to_py)
    }

    fn save_session<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let agent = Arc::clone(&self.inner);
        future_into_py(py, async move {
            agent.save_session().await.map_err(error::agent_error_to_py)
        })
    }
}
