use crate::cli_error::{CliError, unexpected};
#[cfg(test)]
use crate::debug::coding_loop::PERMISSION_NETWORK_SMOKE_ARGV;
use crate::debug::coding_loop::fixture::CodingLoopTaskSmokeFixture;
use crate::debug::coding_loop::{
    CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE, CODING_LOOP_LIVE_SMOKE_TARGET_VALUE,
};
use futures_util::stream;
use merry_core::{ProviderName, ToolName};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelOutput,
    ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse, ModelStreamContext,
    ModelToolCall, ModelToolCallId, ToolArguments,
};
use merry_tool_workspace::{
    CODING_LOOP_PROCESS_TOOL, WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL,
};
use std::sync::Mutex;

pub(crate) struct CodingLoopSmokeProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Mutex<Vec<ModelEvent>>,
}

impl CodingLoopSmokeProvider {
    pub(crate) fn new(relative_cwd: Option<&str>) -> Result<Self, CliError> {
        let steps = vec![
            coding_loop_process_call(
                "coding-loop-smoke-rg-files",
                &["rg", "--files"],
                relative_cwd,
            )?,
            coding_loop_workspace_call(
                "coding-loop-smoke-read",
                WORKSPACE_READ_FILE_TOOL,
                [("path", serde_json::Value::String("src/lib.rs".to_owned()))],
            )?,
            coding_loop_workspace_call(
                "coding-loop-smoke-patch",
                WORKSPACE_PATCH_TOOL,
                [(
                    "patch",
                    serde_json::Value::String(format!(
                        "*** Begin Workspace Patch\n*** Update File: src/lib.rs\n-    \"{CODING_LOOP_LIVE_SMOKE_INITIAL_VALUE}\"\n+    \"{CODING_LOOP_LIVE_SMOKE_TARGET_VALUE}\"\n*** End Workspace Patch"
                    )),
                )],
            )?,
            coding_loop_process_call(
                "coding-loop-smoke-verify",
                &["rg", CODING_LOOP_LIVE_SMOKE_TARGET_VALUE],
                relative_cwd,
            )?,
            ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(
                        "coding-loop-smoke patched greeting and verified it",
                    )],
                    FinishReason::Stop,
                    None,
                ),
            },
        ];

        Ok(Self {
            name: ProviderName::new("merry-coding-loop-smoke-provider").map_err(unexpected)?,
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .map_err(unexpected)?,
            steps: Mutex::new(steps.into_iter().rev().collect()),
        })
    }
}

impl ModelProvider for CodingLoopSmokeProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ModelError::Cancelled);
            }

            let event = self
                .steps
                .lock()
                .expect("coding loop smoke steps mutex should not be poisoned")
                .pop()
                .ok_or_else(|| {
                    ModelError::invalid_request("coding-loop-smoke provider has no scripted step")
                })?;
            Ok(Box::pin(stream::iter([Ok(event)])) as ModelEventStream)
        })
    }
}

#[cfg(test)]
pub(crate) struct PermissionNetworkSmokeProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Mutex<Vec<ModelEvent>>,
}

#[cfg(test)]
impl PermissionNetworkSmokeProvider {
    pub(crate) fn new() -> Result<Self, CliError> {
        let steps = vec![
            coding_loop_process_call(
                "permission-network-smoke-initial-network",
                &PERMISSION_NETWORK_SMOKE_ARGV,
                None,
            )?,
            permission_network_smoke_request_call()?,
            ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(
                        "permission-network-smoke verified approved per-action network access",
                    )],
                    FinishReason::Stop,
                    None,
                ),
            },
        ];

        Ok(Self {
            name: ProviderName::new("merry-permission-network-smoke-provider")
                .map_err(unexpected)?,
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .map_err(unexpected)?,
            steps: Mutex::new(steps.into_iter().rev().collect()),
        })
    }
}

#[cfg(test)]
impl ModelProvider for PermissionNetworkSmokeProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ModelError::Cancelled);
            }

            let event = self
                .steps
                .lock()
                .expect("permission network smoke steps mutex should not be poisoned")
                .pop()
                .ok_or_else(|| {
                    ModelError::invalid_request(
                        "permission-network-smoke provider has no scripted step",
                    )
                })?;
            Ok(Box::pin(stream::iter([Ok(event)])) as ModelEventStream)
        })
    }
}

#[cfg(test)]
pub(crate) struct PermissionNetworkSmokeReviewProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
}

#[cfg(test)]
impl PermissionNetworkSmokeReviewProvider {
    pub(crate) fn new() -> Result<Self, CliError> {
        Ok(Self {
            name: ProviderName::new("merry-permission-network-smoke-review-provider")
                .map_err(unexpected)?,
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .map_err(unexpected)?,
        })
    }
}

#[cfg(test)]
impl ModelProvider for PermissionNetworkSmokeReviewProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ModelError::Cancelled);
            }

            let response = ModelResponse::new(
                vec![ModelOutput::text(
                    r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"low","user_authorization":"high","rationale":"The debug smoke requested network only for the exact DNS lookup that just failed under the inner sandbox."}"#,
                )],
                FinishReason::Stop,
                None,
            );
            Ok(
                Box::pin(stream::iter([Ok(ModelEvent::Completed { response })]))
                    as ModelEventStream,
            )
        })
    }
}

pub(crate) struct CodingLoopTaskSmokeProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Mutex<Vec<ModelEvent>>,
}

impl CodingLoopTaskSmokeProvider {
    pub(crate) fn new(
        relative_cwd: Option<&str>,
        fixture: CodingLoopTaskSmokeFixture,
    ) -> Result<Self, CliError> {
        let steps = vec![
            coding_loop_process_call(
                "coding-loop-task-smoke-rg-files",
                &["rg", "--files"],
                relative_cwd,
            )?,
            coding_loop_process_call(
                "coding-loop-task-smoke-verify-before",
                &["rg", "done", "src/lib.rs"],
                relative_cwd,
            )?,
            coding_loop_workspace_call(
                "coding-loop-task-smoke-read",
                WORKSPACE_READ_FILE_TOOL,
                [("path", serde_json::Value::String("src/lib.rs".to_owned()))],
            )?,
            coding_loop_workspace_call(
                "coding-loop-task-smoke-patch",
                WORKSPACE_PATCH_TOOL,
                [("patch", serde_json::Value::String(fixture.patch_text()))],
            )?,
            coding_loop_process_call(
                "coding-loop-task-smoke-verify-after",
                &["rg", "done", "src/lib.rs"],
                relative_cwd,
            )?,
            ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(
                        "coding-loop-task-smoke fixed the fixture and verified rg done",
                    )],
                    FinishReason::Stop,
                    None,
                ),
            },
        ];

        Ok(Self {
            name: ProviderName::new("merry-coding-loop-task-smoke-provider").map_err(unexpected)?,
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .map_err(unexpected)?,
            steps: Mutex::new(steps.into_iter().rev().collect()),
        })
    }
}

impl ModelProvider for CodingLoopTaskSmokeProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ModelError::Cancelled);
            }

            let event = self
                .steps
                .lock()
                .expect("coding loop task smoke steps mutex should not be poisoned")
                .pop()
                .ok_or_else(|| {
                    ModelError::invalid_request(
                        "coding-loop-task-smoke provider has no scripted step",
                    )
                })?;
            Ok(Box::pin(stream::iter([Ok(event)])) as ModelEventStream)
        })
    }
}

pub(crate) fn coding_loop_process_call(
    call_id: &str,
    argv: &[&str],
    cwd: Option<&str>,
) -> Result<ModelEvent, CliError> {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "argv".to_owned(),
        serde_json::Value::Array(
            argv.iter()
                .map(|argument| serde_json::Value::String((*argument).to_owned()))
                .collect(),
        ),
    );
    if let Some(cwd) = cwd {
        arguments.insert("cwd".to_owned(), serde_json::Value::String(cwd.to_owned()));
    }
    coding_loop_tool_call(call_id, CODING_LOOP_PROCESS_TOOL, arguments)
}

#[cfg(test)]
pub(crate) fn permission_network_smoke_request_call() -> Result<ModelEvent, CliError> {
    let mut requested = serde_json::Map::new();
    requested.insert("network".to_owned(), serde_json::Value::Bool(true));

    let mut for_action = serde_json::Map::new();
    for_action.insert(
        "kind".to_owned(),
        serde_json::Value::String("process".to_owned()),
    );
    for_action.insert(
        "argv".to_owned(),
        serde_json::Value::Array(
            PERMISSION_NETWORK_SMOKE_ARGV
                .iter()
                .map(|argument| serde_json::Value::String((*argument).to_owned()))
                .collect(),
        ),
    );

    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "reason".to_owned(),
        serde_json::Value::String(
            "The same DNS lookup failed under the default inner sandbox; request network for this exact debug smoke command."
                .to_owned(),
        ),
    );
    arguments.insert("requested".to_owned(), serde_json::Value::Object(requested));
    arguments.insert(
        "for_action".to_owned(),
        serde_json::Value::Object(for_action),
    );
    coding_loop_tool_call(
        "permission-network-smoke-request-network",
        "request_permissions",
        arguments,
    )
}

pub(crate) fn coding_loop_workspace_call<const N: usize>(
    call_id: &str,
    tool_name: &str,
    arguments: [(&str, serde_json::Value); N],
) -> Result<ModelEvent, CliError> {
    coding_loop_tool_call(
        call_id,
        tool_name,
        arguments
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

pub(crate) fn coding_loop_tool_call(
    call_id: &str,
    tool_name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<ModelEvent, CliError> {
    let call = ModelToolCall::new(
        ModelToolCallId::new(call_id).map_err(unexpected)?,
        ToolName::new(tool_name).map_err(unexpected)?,
        ToolArguments::new(arguments),
    );
    Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    })
}
