//! PyO3 adapter for the built Rust agent.

use crate::{error, interactive::PyInteractiveRun, protocol, run::PyAgentRun};
use merry::Agent;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::sync::Arc;

/// Built Rust-owned agent handle.
#[pyclass(name = "Agent")]
pub(crate) struct PyAgent {
    inner: Arc<Agent>,
    session_store_path: Option<String>,
}

impl PyAgent {
    pub(crate) fn new(agent: Agent, session_store_path: Option<String>) -> Self {
        Self {
            inner: Arc::new(agent),
            session_store_path,
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
        py: Python<'_>,
        task: String,
        final_output_schema_json: Option<String>,
    ) -> PyResult<PyAgentRun> {
        let agent = Arc::clone(&self.inner);
        let schema = final_output_schema_json
            .as_deref()
            .map(protocol::parse_input_schema)
            .transpose()
            .map_err(error::protocol_message_to_py)?;
        let result = py.detach(move || {
            let _guard = pyo3_async_runtimes::tokio::get_runtime().enter();
            match schema {
                Some(schema) => agent.stream_with_owned_tool_handoff_and_schema(&task, schema),
                None => agent.stream_with_owned_tool_handoff(&task),
            }
        });
        result
            .map(PyAgentRun::new)
            .map_err(error::agent_error_to_py)
    }

    fn start_interactive(&self, py: Python<'_>) -> PyResult<PyInteractiveRun> {
        let agent = Arc::clone(&self.inner);
        let session_store_path = self.session_store_path.clone();
        let result = py.detach(move || {
            let _guard = pyo3_async_runtimes::tokio::get_runtime().enter();
            agent.start_interactive_owned()
        });
        result
            .map(|run| PyInteractiveRun::new(run, session_store_path))
            .map_err(error::agent_error_to_py)
    }

    fn save_session<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let agent = Arc::clone(&self.inner);
        future_into_py(py, async move {
            agent.save_session().await.map_err(error::agent_error_to_py)
        })
    }

    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let agent = Arc::clone(&self.inner);
        future_into_py(py, async move {
            agent.close().await.map_err(error::agent_error_to_py)
        })
    }
}
