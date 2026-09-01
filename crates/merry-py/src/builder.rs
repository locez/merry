//! PyO3 adapter for typed agent construction.

use crate::{agent::PyAgent, error, protocol};
use merry::profiles::WorkspaceToolLimits;
use merry::providers::OpenAiProtocol;
use merry::{AgentBuilder, AgentLoopConfig, FileSessionStore};
use merry_core::ToolName;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::{num::NonZeroUsize, path::PathBuf};

/// Mutable construction state exposed to Python before an agent is built.
#[pyclass(name = "AgentBuilder")]
pub(crate) struct PyAgentBuilder {
    inner: Option<AgentBuilder>,
    session_store_path: Option<String>,
}

#[pymethods]
impl PyAgentBuilder {
    #[new]
    fn new(session_id: String) -> PyResult<Self> {
        let session_id = merry::SessionId::new(&session_id)
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
        Ok(Self {
            inner: Some(AgentBuilder::new(session_id)),
            session_store_path: None,
        })
    }

    fn with_openai_compatible(
        &mut self,
        api_key: String,
        model: String,
        base_url: Option<String>,
        protocol: Option<String>,
    ) -> PyResult<()> {
        let mut provider_config = merry::providers::OpenAiProviderConfig::new(&api_key)
            .map_err(|error| error::provider_message_to_py(error.to_string()))?;
        if let Some(base_url) = base_url.as_deref() {
            provider_config = provider_config
                .with_base_url(base_url)
                .map_err(|error| error::provider_message_to_py(error.to_string()))?;
        }
        if let Some(protocol) = protocol.as_deref() {
            provider_config = provider_config.with_protocol(parse_openai_protocol(protocol)?);
        }
        let provider = merry::providers::openai_compatible()
            .provider_config(provider_config)
            .model_name(&model)
            .map_err(|error| error::provider_message_to_py(error.to_string()))?
            .build()
            .map_err(|error| error::provider_message_to_py(error.to_string()))?;
        let builder = self.take_builder()?;
        self.inner = Some(builder.provider(provider));
        Ok(())
    }

    fn with_anthropic(
        &mut self,
        api_key: String,
        model: String,
        base_url: Option<String>,
    ) -> PyResult<()> {
        let mut provider_config = merry::providers::AnthropicProviderConfig::new(&api_key)
            .map_err(|error| error::provider_message_to_py(error.to_string()))?;
        if let Some(base_url) = base_url.as_deref() {
            provider_config = provider_config
                .with_base_url(base_url)
                .map_err(|error| error::provider_message_to_py(error.to_string()))?;
        }
        let provider = merry::providers::anthropic()
            .provider_config(provider_config)
            .model_name(&model)
            .map_err(|error| error::provider_message_to_py(error.to_string()))?
            .build()
            .map_err(|error| error::provider_message_to_py(error.to_string()))?;
        let builder = self.take_builder()?;
        self.inner = Some(builder.provider(provider));
        Ok(())
    }

    // Keep the PyO3 ABI scalar-only; Python groups these values in WorkspaceLimits.
    #[allow(clippy::too_many_arguments)]
    fn with_workspace(
        &mut self,
        roots: Vec<String>,
        readonly_resource_roots: Vec<String>,
        allow_hidden: bool,
        enable_patch: bool,
        patch_write_scope: Option<Vec<String>>,
        forbidden_paths: Vec<String>,
        max_read_bytes: usize,
        max_write_bytes: usize,
        max_patch_bytes: usize,
        max_list_entries: usize,
        max_search_matches: usize,
        max_search_files: usize,
        max_search_entries: usize,
        max_search_bytes: usize,
        max_search_line_bytes: usize,
        max_search_query_bytes: usize,
    ) -> PyResult<()> {
        if roots.is_empty() {
            return Err(error::config_message_to_py(
                "workspace requires at least one root",
            ));
        }
        let mut profile_builder = merry::profiles::CodingAgentProfileBuilder::with_roots(
            roots.into_iter().map(PathBuf::from),
        )
        .readonly_resource_roots(readonly_resource_roots.into_iter().map(PathBuf::from))
        .allow_hidden(allow_hidden)
        .limits(WorkspaceToolLimits {
            max_read_bytes,
            max_write_bytes,
            max_patch_bytes,
            max_list_entries,
            max_search_matches,
            max_search_files,
            max_search_entries,
            max_search_bytes,
            max_search_line_bytes,
            max_search_query_bytes,
        })
        .forbidden_paths(forbidden_paths.into_iter().map(PathBuf::from));
        if enable_patch {
            profile_builder = profile_builder.patch_tool();
            if let Some(scope) = patch_write_scope {
                profile_builder =
                    profile_builder.patch_write_scope(scope.into_iter().map(PathBuf::from));
            }
        } else if patch_write_scope.is_some() {
            return Err(error::config_message_to_py(
                "patch_write_scope requires enable_patch=true",
            ));
        }
        let profile = profile_builder
            .build()
            .map_err(error::profile_build_error_to_py)?;
        let builder = self.take_builder()?;
        self.inner = Some(
            builder
                .profile(profile)
                .map_err(error::agent_build_error_to_py)?,
        );
        Ok(())
    }

    fn register_bridge_tool(
        &mut self,
        name: String,
        description: String,
        schema_json: String,
    ) -> PyResult<()> {
        let name =
            ToolName::new(&name).map_err(|error| error::config_message_to_py(error.to_string()))?;
        let schema =
            protocol::parse_input_schema(&schema_json).map_err(error::protocol_message_to_py)?;
        let spec = merry_core::ToolSpec::new(name, &description, schema)
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
        let builder = self.take_builder()?;
        self.inner = Some(builder.bridge_tool_spec(spec));
        Ok(())
    }

    fn with_session_store(&mut self, path: String) -> PyResult<()> {
        let session_store_path = path.clone();
        let builder = self.take_builder()?;
        self.inner = Some(builder.session_store(FileSessionStore::new(path)));
        self.session_store_path = Some(session_store_path);
        Ok(())
    }

    fn with_max_model_turns(&mut self, max_model_turns: usize) -> PyResult<()> {
        let config = AgentLoopConfig::new(max_model_turns)
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
        let builder = self.take_builder()?;
        self.inner = Some(builder.loop_config(config));
        Ok(())
    }

    fn with_event_buffer_size(&mut self, size: usize) -> PyResult<()> {
        let size = NonZeroUsize::new(size).ok_or_else(|| {
            error::config_message_to_py("event buffer size must be greater than zero")
        })?;
        let builder = self.take_builder()?;
        self.inner = Some(builder.event_buffer_size(size));
        Ok(())
    }

    fn build(&mut self) -> PyResult<PyAgent> {
        let builder = self.take_builder()?;
        let agent = builder.build().map_err(error::agent_build_error_to_py)?;
        Ok(PyAgent::new(agent))
    }

    #[doc(hidden)]
    fn is_consumed(&self) -> bool {
        self.inner.is_none()
    }

    fn resume<'py>(
        &mut self,
        py: Python<'py>,
        path: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let path = path
            .or_else(|| self.session_store_path.clone())
            .ok_or_else(|| {
                error::config_message_to_py(
                    "resume requires a path or a previously configured session store",
                )
            })?;
        let builder = self.take_builder()?;
        let store = FileSessionStore::new(path);
        future_into_py(py, async move {
            let result = builder.resume_from_store(store).await;
            result
                .map(PyAgent::new)
                .map_err(error::agent_build_error_to_py)
        })
    }
}

impl PyAgentBuilder {
    fn take_builder(&mut self) -> PyResult<AgentBuilder> {
        self.inner.take().ok_or_else(error::builder_consumed_to_py)
    }
}

fn parse_openai_protocol(value: &str) -> PyResult<OpenAiProtocol> {
    match value {
        "responses" => Ok(OpenAiProtocol::Responses),
        "chat_completions" | "chat-completions" => Ok(OpenAiProtocol::ChatCompletions),
        _ => Err(error::config_message_to_py(format!(
            "unsupported OpenAI-compatible protocol: {value}"
        ))),
    }
}
