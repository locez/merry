//! PyO3 adapter for typed agent construction.

use crate::{agent::PyAgent, error, protocol};
use merry::profiles::WorkspaceToolLimits;
use merry::providers::OpenAiProtocol;
use merry::{
    AgentBuilder, AgentLoopConfig, ArchiveConfig, FileSessionStore, GenerationConfig,
    HistoryRetention, ParallelToolCalls, PromptBlock, PromptProfile, ReasoningEffort, ServiceTier,
};
use merry_core::ToolName;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::{
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
};

/// Mutable construction state exposed to Python before an agent is built.
#[pyclass(name = "AgentBuilder")]
pub(crate) struct PyAgentBuilder {
    inner: Option<AgentBuilder>,
    session_store_path: Option<String>,
    prompt_profile: Option<PromptProfile>,
    progress_commentary: Option<bool>,
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
            prompt_profile: None,
            progress_commentary: None,
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

    fn with_prompt_profile(
        &mut self,
        base_instructions: Option<String>,
        progress_commentary: Option<bool>,
        progress_commentary_instructions: Option<String>,
        stable_blocks: Vec<(String, String)>,
    ) -> PyResult<()> {
        if self.inner.is_none() {
            return Err(error::builder_consumed_to_py());
        }
        let mut profile = PromptProfile::default();
        if let Some(base) = base_instructions {
            profile = profile
                .with_base_instructions(base)
                .map_err(|error| error::config_message_to_py(error.to_string()))?;
        }
        if let Some(commentary) = progress_commentary_instructions {
            profile = profile
                .with_progress_commentary_instructions(commentary)
                .map_err(|error| error::config_message_to_py(error.to_string()))?;
        }
        for (tag, text) in stable_blocks {
            let block = PromptBlock::new(tag, text)
                .map_err(|error| error::config_message_to_py(error.to_string()))?;
            profile = profile
                .with_stable_block(block)
                .map_err(|error| error::config_message_to_py(error.to_string()))?;
        }
        self.prompt_profile = Some(profile);
        self.progress_commentary = progress_commentary;
        Ok(())
    }

    fn with_generation_config(
        &mut self,
        max_output_tokens: Option<u64>,
        parallel_tool_calls: Option<String>,
        reasoning_effort: Option<String>,
        service_tier: Option<String>,
    ) -> PyResult<()> {
        let parallel = match parallel_tool_calls.as_deref() {
            None | Some("auto") => ParallelToolCalls::Auto,
            Some("enabled") => ParallelToolCalls::Enabled,
            Some("disabled") => ParallelToolCalls::Disabled,
            Some(other) => {
                return Err(error::config_message_to_py(format!(
                    "unsupported parallel_tool_calls mode: {other}"
                )));
            }
        };
        let reasoning = reasoning_effort
            .as_deref()
            .map(ReasoningEffort::new)
            .transpose()
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
        let tier = service_tier
            .as_deref()
            .map(ServiceTier::new)
            .transpose()
            .map_err(|error| error::config_message_to_py(error.to_string()))?;
        let config = GenerationConfig::new(max_output_tokens, false)
            .map_err(|error| error::config_message_to_py(error.to_string()))?
            .with_parallel_tool_calls(parallel)
            .with_reasoning_effort(reasoning)
            .with_service_tier(tier);
        let builder = self.take_builder()?;
        self.inner = Some(builder.generation_config(config));
        Ok(())
    }

    // Keep the PyO3 ABI scalar-only; Python groups these values in WorkspaceLimits.
    #[allow(clippy::too_many_arguments)]
    fn with_workspace(
        &mut self,
        root: String,
        readonly_resource_roots: Vec<String>,
        enable_patch: bool,
        patch_write_scope: Option<Vec<String>>,
        forbidden_paths: Vec<String>,
        max_read_bytes: Option<usize>,
        max_read_lines: Option<usize>,
        max_write_bytes: Option<usize>,
        max_patch_bytes: Option<usize>,
    ) -> PyResult<()> {
        if root.is_empty() {
            return Err(error::config_message_to_py("workspace requires a root"));
        }
        let defaults = WorkspaceToolLimits::default();
        let mut profile_builder =
            merry::profiles::CodingAgentProfileBuilder::new(PathBuf::from(root))
                .readonly_resource_roots(readonly_resource_roots.into_iter().map(PathBuf::from))
                .limits(WorkspaceToolLimits {
                    max_read_bytes: max_read_bytes.unwrap_or(defaults.max_read_bytes),
                    max_read_lines: max_read_lines.unwrap_or(defaults.max_read_lines),
                    max_write_bytes: max_write_bytes.unwrap_or(defaults.max_write_bytes),
                    max_patch_bytes: max_patch_bytes.unwrap_or(defaults.max_patch_bytes),
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

    fn with_history_retention(
        &mut self,
        mode: &str,
        max_segments: Option<usize>,
        max_bytes: Option<u64>,
        export_markdown: Option<bool>,
    ) -> PyResult<()> {
        let retention = parse_history_retention(mode, max_segments, max_bytes, export_markdown)?;
        let builder = self.take_builder()?;
        self.inner = Some(builder.history_retention(retention));
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
        let builder = self.take_finalized_builder()?;
        let session_store_path = self.session_store_path.clone();
        let agent = builder.build().map_err(error::agent_build_error_to_py)?;
        Ok(PyAgent::new(agent, session_store_path))
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
        let resolved_path = path
            .or_else(|| self.session_store_path.clone())
            .ok_or_else(|| {
                error::config_message_to_py(
                    "resume requires a path or a previously configured session store",
                )
            })?;
        let builder = self.take_finalized_builder()?;
        let session_store_path = self
            .session_store_path
            .clone()
            .or_else(|| Some(resolved_path.clone()));
        let store = FileSessionStore::new(resolved_path);
        future_into_py(py, async move {
            let result = builder.resume_from_store(store).await;
            result
                .map(|agent| PyAgent::new(agent, session_store_path))
                .map_err(error::agent_build_error_to_py)
        })
    }
}

impl PyAgentBuilder {
    fn take_builder(&mut self) -> PyResult<AgentBuilder> {
        self.inner.take().ok_or_else(error::builder_consumed_to_py)
    }

    fn take_finalized_builder(&mut self) -> PyResult<AgentBuilder> {
        let mut builder = self.take_builder()?;
        if let Some(profile) = self.prompt_profile.take() {
            builder = builder.prompt_profile(profile);
        }
        if let Some(enabled) = self.progress_commentary.take() {
            builder = builder.progress_commentary(enabled);
        }
        Ok(builder)
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

fn parse_history_retention(
    mode: &str,
    max_segments: Option<usize>,
    max_bytes: Option<u64>,
    export_markdown: Option<bool>,
) -> PyResult<HistoryRetention> {
    match mode {
        "archive" => {
            let max_segments = max_segments
                .map(|value| {
                    NonZeroUsize::new(value).ok_or_else(|| {
                        error::config_message_to_py("max_segments must be greater than zero")
                    })
                })
                .transpose()?;
            let max_bytes = max_bytes
                .map(|value| {
                    NonZeroU64::new(value).ok_or_else(|| {
                        error::config_message_to_py("max_bytes must be greater than zero")
                    })
                })
                .transpose()?;
            let config = ArchiveConfig::new()
                .with_max_segments(max_segments)
                .with_max_bytes(max_bytes)
                .with_export_markdown(export_markdown.unwrap_or(true));
            Ok(HistoryRetention::Archive(config))
        }
        "context_only" => {
            if max_segments.is_some() || max_bytes.is_some() || export_markdown == Some(true) {
                return Err(error::config_message_to_py(
                    "max_segments, max_bytes, and export_markdown are only valid for archive retention",
                ));
            }
            Ok(HistoryRetention::ContextOnly)
        }
        other => Err(error::config_message_to_py(format!(
            "unsupported history_retention mode: {other}"
        ))),
    }
}
