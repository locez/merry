use crate::cli_error::{CliError, debug_openai_usage_error, stdout_error, unexpected};
use crate::config::MerryConfig;
use crate::provider_config::{
    MERRY_OPENAI_DEBUG_ENV, OpenAiRuntimeConfig, apply_openai_context_compaction_provider,
    openai_runtime_config, optional_env,
};
use crate::runtime_config::configured_runtime_builder;
use crate::runtime_events::{
    first_pending_tool_call, write_runtime_events, write_runtime_step_events,
    write_runtime_step_events_to,
};
use crate::{DEBUG_TOOL_CONTINUATION_INPUT, DEBUG_TOOL_NAME, DEFAULT_SESSION_ID};
use merry_core::{PendingToolCall, SessionId, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{GenerationConfig, ModelName};
use merry_provider_openai::OpenAiProvider;
use merry_runtime::{
    RegisteredTool, Runtime, StepContext, StepInput, ToolExecutionContext, ToolExecutionOutcome,
    ToolExecutor, ToolExecutorFuture,
};
use std::sync::Arc;
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

#[cfg(test)]
mod tests;

pub(crate) async fn run(
    input: &str,
    model: Option<&str>,
    max_output_tokens: Option<u64>,
    debug_tool_result: Option<&str>,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let config = debug_config(model, merry_config)?;

    let session_id = SessionId::new(DEFAULT_SESSION_ID).map_err(debug_openai_usage_error)?;
    let model = ModelName::new(&config.primary.model).map_err(debug_openai_usage_error)?;
    let provider = OpenAiProvider::new(config.primary.provider);
    let mut builder = configured_runtime_builder(session_id, merry_config)?
        .model_provider(Arc::new(provider), model);
    builder = apply_openai_context_compaction_provider(builder, config.context_compaction)?;
    if let Some(result) = debug_tool_result {
        builder = builder.register_tool(echo_tool(result)?);
    }
    let runtime = builder.build().map_err(unexpected)?;
    let input = StepInput::user_text(input).map_err(debug_openai_usage_error)?;
    let generation_config =
        GenerationConfig::new(max_output_tokens, false).map_err(debug_openai_usage_error)?;
    let context = StepContext::new(Default::default()).with_generation_config(generation_config);

    if debug_tool_result.is_some() {
        write_tool_events(&runtime, input, context, tokio::io::stdout()).await
    } else {
        write_runtime_step_events(&runtime, input, context, tokio::io::stdout()).await
    }
}

pub(crate) async fn write_tool_events<W>(
    runtime: &Runtime,
    input: StepInput,
    context: StepContext,
    writer: W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    let events = write_runtime_step_events_to(runtime, input, context.clone(), &mut writer).await?;
    let pending = first_pending_tool_call(&events);

    let Some(pending) = pending else {
        writer.flush().await.map_err(stdout_error)?;
        return Err(CliError::Unexpected(format!(
            "debug tool `{DEBUG_TOOL_NAME}` was not called on the first step; no tool call was pending"
        )));
    };

    let actual_tool_name = pending.name().as_str();
    if actual_tool_name != DEBUG_TOOL_NAME {
        writer.flush().await.map_err(stdout_error)?;
        return Err(CliError::Unexpected(format!(
            "debug tool `{DEBUG_TOOL_NAME}` was not called on the first step; pending tool was `{actual_tool_name}`"
        )));
    }

    write_runtime_events(
        runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .map_err(unexpected)?,
        &mut writer,
    )
    .await?;

    let input = StepInput::user_text(DEBUG_TOOL_CONTINUATION_INPUT).map_err(unexpected)?;
    write_runtime_step_events_to(runtime, input, context, &mut writer).await?;

    writer.flush().await.map_err(stdout_error)
}

pub(crate) fn echo_tool(result: &str) -> Result<RegisteredTool, CliError> {
    if result.trim().is_empty() {
        return Err(debug_openai_usage_error(
            "--debug-tool-result must not be blank",
        ));
    }

    let schema = serde_json::from_value::<ToolInputSchema>(serde_json::json!({
        "type": "object",
        "additionalProperties": true
    }))
    .map_err(debug_openai_usage_error)?;
    let spec = ToolSpec::new(
        ToolName::new(DEBUG_TOOL_NAME).map_err(debug_openai_usage_error)?,
        "Return the fixed debug text provided by the CLI.",
        schema,
    )
    .map_err(debug_openai_usage_error)?;

    Ok(RegisteredTool::read_only(
        spec,
        Arc::new(DebugEchoExecutor {
            result: result.to_owned(),
        }),
    ))
}

pub(crate) fn debug_config(
    model_flag: Option<&str>,
    merry_config: Option<&MerryConfig>,
) -> Result<OpenAiRuntimeConfig, CliError> {
    config_with_env(model_flag, merry_config, optional_env)
}

pub(crate) fn config_with_env(
    model_flag: Option<&str>,
    merry_config: Option<&MerryConfig>,
    env_value: impl Fn(&'static str) -> Result<Option<String>, CliError>,
) -> Result<OpenAiRuntimeConfig, CliError> {
    if env_value(MERRY_OPENAI_DEBUG_ENV)?.as_deref() != Some("1") {
        return Err(debug_openai_usage_error(
            "set MERRY_OPENAI_DEBUG=1 to enable live OpenAI-compatible debugging",
        ));
    }

    openai_runtime_config(model_flag, merry_config, debug_openai_usage_error)
}

struct DebugEchoExecutor {
    result: String,
}

impl ToolExecutor for DebugEchoExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(ToolExecutionOutcome::succeeded_text(self.result.clone())) })
    }
}
