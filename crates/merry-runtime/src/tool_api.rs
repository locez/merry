//! Application-facing typed tool construction.
//!
//! This module owns the small semantic tool API used by the facade and coding
//! composition layers. The runtime converts a typed handler into its internal
//! [`RegisteredTool`] representation; application code does not need to know
//! about executors, action categories, or runner selection.

use crate::{
    FINAL_OUTPUT_TOOL_NAME, RegisteredTool, ToolActionKind, ToolExecutionContext,
    ToolExecutionError, ToolExecutionOutcome, ToolExecutionResult, ToolExecutor,
    ToolExecutorFuture,
};
use merry_core::{CoreError, ErrorInfo, PendingToolCall, ToolInputSchema, ToolName, ToolSpec};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use std::{fmt, future::Future, marker::PhantomData, sync::Arc};
use thiserror::Error;

/// A typed application tool executed by the Merry runtime.
///
/// The tool's input schema is derived from `I`. The handler runs as trusted
/// in-process application code; runtime owns invocation ordering, lifecycle,
/// artifact recording, and model continuation. Use the explicit host handoff
/// protocol only when a different language runtime owns the callable.
#[derive(Clone)]
pub struct Tool {
    spec: ToolSpec,
    executor: Arc<dyn ToolExecutor>,
}

impl Tool {
    /// Creates a typed asynchronous application tool.
    ///
    /// Handler errors become durable failed tool results, allowing the model
    /// loop to observe the failure and recover. Serialization or argument
    /// decoding failures are also represented as tool failures where the call
    /// has already been admitted by runtime.
    pub fn new<I, O, F, Fut, E>(
        name: impl AsRef<str>,
        description: impl AsRef<str>,
        handler: F,
    ) -> Result<Self, ToolBuildError>
    where
        I: DeserializeOwned + JsonSchema + Send + 'static,
        O: Serialize + Send + 'static,
        F: Fn(I) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O, E>> + Send + 'static,
        E: fmt::Display + Send + Sync + 'static,
    {
        let name = ToolName::new(name.as_ref())?;
        if name.as_str() == FINAL_OUTPUT_TOOL_NAME {
            return Err(ToolBuildError::ReservedName { name });
        }
        let input_schema = ToolInputSchema::new(schemars::schema_for!(I))?.require_object()?;
        let spec = ToolSpec::new(name, description.as_ref(), input_schema)?;
        let executor = TypedToolExecutor {
            handler,
            input_marker: PhantomData,
            future_marker: PhantomData,
            error_marker: PhantomData,
        };

        Ok(Self {
            spec,
            executor: Arc::new(executor),
        })
    }

    /// Borrows the provider-neutral tool specification.
    #[must_use]
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// Returns the internal runtime registration used by coding composition.
    ///
    /// This is a framework integration hook. Application code should pass the
    /// [`Tool`] to `CodingAgentProfileBuilder::tool` or `AgentBuilder::tool`.
    #[doc(hidden)]
    #[must_use]
    pub fn into_registered_tool(self) -> RegisteredTool {
        RegisteredTool::new(self.spec, self.executor, ToolActionKind::TrustedExternal)
    }
}

impl fmt::Debug for Tool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Tool")
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}

/// Errors raised while constructing a typed application tool.
#[derive(Debug, Error)]
pub enum ToolBuildError {
    /// The public tool name, description, or schema was invalid.
    #[error(transparent)]
    Core(#[from] CoreError),
    /// The tool name is reserved for runtime-owned protocol behavior.
    #[error("tool name {name} is reserved by the runtime")]
    ReservedName {
        /// Rejected reserved tool name.
        name: ToolName,
    },
}

struct TypedToolExecutor<I, O, F, Fut, E> {
    handler: F,
    input_marker: PhantomData<fn(I) -> O>,
    future_marker: PhantomData<fn() -> Fut>,
    error_marker: PhantomData<fn() -> E>,
}

impl<I, O, F, Fut, E> ToolExecutor for TypedToolExecutor<I, O, F, Fut, E>
where
    I: DeserializeOwned + JsonSchema + Send + 'static,
    O: Serialize + Send + 'static,
    F: Fn(I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O, E>> + Send + 'static,
    E: fmt::Display + Send + Sync + 'static,
{
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        let input = match serde_json::from_value(serde_json::Value::Object(
            call.arguments().as_object().clone(),
        )) {
            Ok(input) => input,
            Err(source) => {
                return Box::pin(async move {
                    failed_tool_result(
                        format!("tool arguments could not be decoded: {source}"),
                        "tool_input_decode_failed",
                        "tool arguments did not match the declared input type",
                    )
                });
            }
        };

        let cancellation_token = context.cancellation_token().clone();
        let handler_future = (self.handler)(input);
        Box::pin(async move {
            tokio::select! {
                result = handler_future => match result {
                    Ok(output) => match serde_json::to_string(&output) {
                        Ok(content) => Ok(ToolExecutionOutcome::succeeded_json(content)),
                        Err(source) => failed_tool_result(
                            format!("tool output could not be serialized: {source}"),
                            "tool_output_serialization_failed",
                            "the tool returned a value that could not be serialized",
                        ),
                    },
                    Err(error) => failed_tool_result(
                        error.to_string(),
                        "tool_handler_failed",
                        "the tool handler returned a domain error",
                    ),
                },
                _ = cancellation_token.cancelled() => Err(ToolExecutionError::Cancelled),
            }
        })
    }
}

fn failed_tool_result(
    content: String,
    code: &'static str,
    diagnostic_message: &'static str,
) -> ToolExecutionResult {
    let diagnostic = ErrorInfo::new(code, diagnostic_message).map_err(|source| {
        ToolExecutionError::infrastructure(format!(
            "failed to create tool failure diagnostic: {source}"
        ))
    })?;
    Ok(ToolExecutionOutcome::failed_text(content, diagnostic))
}
