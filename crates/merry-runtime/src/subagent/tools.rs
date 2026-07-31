use super::{
    CANCEL_SUBAGENTS_TOOL_NAME, SPAWN_SUBAGENTS_TOOL_NAME, SubagentError, SubagentManager,
    SubagentTaskSpec, WAIT_SUBAGENTS_TOOL_NAME, WaitMode,
    protocol::{
        CancelSubagentsInput, SpawnSubagentTaskInput, SpawnSubagentsInput, WaitSubagentsInput,
    },
    sanitize_diagnostic_message,
};
use crate::plan::projection::{CHILD_LINKED_SCOPE_GUIDANCE, LINKED_CHILD_DECOMPOSITION_GUIDANCE};
use crate::{
    RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome,
    ToolExecutionResult, ToolExecutor, ToolExecutorFuture,
};
use merry_core::{ErrorInfo, PendingToolCall, ToolInputSchema, ToolName, ToolSpec};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

const WAIT_SEMANTIC_CHECKPOINT_GUIDANCE: &str = "Observe child statuses at semantic or terminal checkpoints; do not poll for high-frequency progress.";

/// Returns provider-visible subagent tool specs.
pub fn subagent_tool_specs() -> Result<[ToolSpec; 3], merry_core::CoreError> {
    subagent_tool_specs_with_default(super::DEFAULT_MAX_MODEL_TURNS)
}

fn subagent_tool_specs_with_default(
    default_max_model_turns: u32,
) -> Result<[ToolSpec; 3], merry_core::CoreError> {
    let mut spawn_schema = schemars::schema_for!(SpawnSubagentsInput);
    if !set_max_model_turns_schema_default(&mut spawn_schema, default_max_model_turns) {
        return Err(merry_core::CoreError::InvalidSchema {
            kind: "SpawnSubagentsInput",
            reason: "generated schema is missing the max_model_turns property",
        });
    }

    Ok([
        tool_spec_from_schema(
            SPAWN_SUBAGENTS_TOOL_NAME,
            &format!(
                "Spawn bounded child agents for parallel delegated tasks. A terminal child result is delivered to the parent as a runtime update on the next model turn; it does not interrupt the current turn. Review that update before claiming the parent task is complete, and call wait_subagents when the compact result, diagnostics, or changed paths are needed. Omit max_model_turns to use the configured runtime default ({default_max_model_turns} unless changed by runtime policy). Use the structured scope fields when a child needs narrower boundaries; keep tasks[].task focused on the work and do not repeat scope declarations in task text. When binding a child to an authored Plan node, set plan_client_key to one of the authored strings in update_plan.bindable_plan_client_keys, such as `agent1_task`; never pass a runtime node id such as `plan-node-2`. When plan_client_key binds a child: {CHILD_LINKED_SCOPE_GUIDANCE} {LINKED_CHILD_DECOMPOSITION_GUIDANCE} In tasks[].allowed_tools, copy exact registered Merry tool names without provider namespace prefixes: use run_process, never functions.run_process."
            ),
            spawn_schema,
        )?,
        tool_spec::<WaitSubagentsInput>(
            WAIT_SUBAGENTS_TOOL_NAME,
            format!(
                "Inspect or wait for child agent statuses and compact results. {WAIT_SEMANTIC_CHECKPOINT_GUIDANCE} timeout_ms is an observation deadline, not a task budget; zero returns an immediate status snapshot, while omission waits for the selected completion condition. A timed_out=true result is only a status snapshot, never completion. Claim completion only when terminal=true and the relevant statuses are terminal."
            ),
        )?,
        tool_spec::<CancelSubagentsInput>(
            CANCEL_SUBAGENTS_TOOL_NAME,
            "Cancel selected child agents.",
        )?,
    ])
}

/// Returns provider-visible subagent tool specs with runtime-owned executors.
pub fn subagent_registered_tools(
    manager: SubagentManager,
) -> Result<[RegisteredTool; 3], merry_core::CoreError> {
    let [spawn_spec, wait_spec, cancel_spec] =
        subagent_tool_specs_with_default(manager.max_model_turns())?;
    Ok([
        RegisteredTool::new(
            spawn_spec,
            Arc::new(SpawnSubagentsExecutor::new(manager.clone())),
            ToolActionKind::RuntimeControl,
        ),
        RegisteredTool::read_only(
            wait_spec,
            Arc::new(WaitSubagentsExecutor::new(manager.clone())),
        ),
        RegisteredTool::new(
            cancel_spec,
            Arc::new(CancelSubagentsExecutor::new(manager)),
            ToolActionKind::RuntimeControl,
        ),
    ])
}

/// Runtime-owned executor for the provider-visible `spawn_subagents` tool.
#[derive(Clone)]
struct SpawnSubagentsExecutor {
    manager: SubagentManager,
}

impl SpawnSubagentsExecutor {
    /// Creates a spawn executor backed by the shared subagent manager.
    #[must_use]
    fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for SpawnSubagentsExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input = match input_from_call::<SpawnSubagentsInput>(&call) {
                Ok(input) => input,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            let tasks = match input
                .tasks
                .into_iter()
                .map(|task| task_spec_from_input(task, self.manager.max_model_turns()))
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(tasks) => tasks,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            let output = self
                .manager
                .spawn(
                    tasks,
                    input.max_concurrency,
                    context.cancellation_token().clone(),
                )
                .await
                .map_err(infrastructure_error)?;

            spawn_json_output(&output)
        })
    }
}

/// Runtime-owned executor for the provider-visible `wait_subagents` tool.
#[derive(Clone)]
struct WaitSubagentsExecutor {
    manager: SubagentManager,
}

impl WaitSubagentsExecutor {
    /// Creates a wait executor backed by the shared subagent manager.
    #[must_use]
    fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for WaitSubagentsExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input = match input_from_call::<WaitSubagentsInput>(&call) {
                Ok(input) => input,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            if input.agent_ids.is_empty() {
                return Ok(invalid_subagent_arguments_outcome(
                    call.name().as_str(),
                    InvalidSubagentToolArguments::new(
                        "agent_ids must contain at least one child agent id",
                    ),
                ));
            }
            let timeout = input.timeout_ms.map(Duration::from_millis);
            let wait = self.manager.wait(
                &input.agent_ids,
                input.mode.unwrap_or(WaitMode::All),
                timeout,
            );
            let output = tokio::select! {
                biased;
                () = context.cancellation_token().cancelled() => {
                    return Err(ToolExecutionError::Cancelled);
                }
                output = wait => output.map_err(infrastructure_error)?,
            };

            succeeded_json_output(WAIT_SUBAGENTS_TOOL_NAME, &output)
        })
    }
}

/// Runtime-owned executor for the provider-visible `cancel_subagents` tool.
#[derive(Clone)]
struct CancelSubagentsExecutor {
    manager: SubagentManager,
}

impl CancelSubagentsExecutor {
    /// Creates a cancel executor backed by the shared subagent manager.
    #[must_use]
    fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for CancelSubagentsExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input = match input_from_call::<CancelSubagentsInput>(&call) {
                Ok(input) => input,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            if input.agent_ids.is_empty() {
                return Ok(invalid_subagent_arguments_outcome(
                    call.name().as_str(),
                    InvalidSubagentToolArguments::new(
                        "agent_ids must contain at least one child agent id",
                    ),
                ));
            }
            let output = self
                .manager
                .cancel(&input.agent_ids)
                .await
                .map_err(infrastructure_error)?;

            succeeded_json_output(CANCEL_SUBAGENTS_TOOL_NAME, &output)
        })
    }
}

fn tool_spec<T>(
    name: &str,
    description: impl Into<String>,
) -> Result<ToolSpec, merry_core::CoreError>
where
    T: JsonSchema,
{
    let description = description.into();
    tool_spec_from_schema(name, &description, schemars::schema_for!(T))
}

fn tool_spec_from_schema(
    name: &str,
    description: &str,
    schema: schemars::Schema,
) -> Result<ToolSpec, merry_core::CoreError> {
    ToolSpec::new(
        ToolName::new(name)?,
        description,
        ToolInputSchema::new(schema)?,
    )
}

fn set_max_model_turns_schema_default(
    schema: &mut schemars::Schema,
    default_max_model_turns: u32,
) -> bool {
    set_max_model_turns_schema_default_in_value(schema.as_object_mut(), default_max_model_turns)
}

fn set_max_model_turns_schema_default_in_value(
    object: Option<&mut serde_json::Map<String, Value>>,
    default_max_model_turns: u32,
) -> bool {
    let Some(object) = object else {
        return false;
    };

    let mut found = false;
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut)
        && let Some(field) = properties
            .get_mut("max_model_turns")
            .and_then(Value::as_object_mut)
    {
        field.insert(
            "default".to_owned(),
            serde_json::json!(default_max_model_turns),
        );
        found = true;
    }

    for child in object.values_mut() {
        if set_max_model_turns_schema_default_in_value(
            child.as_object_mut(),
            default_max_model_turns,
        ) {
            found = true;
        }
    }
    found
}

fn input_from_call<T>(call: &PendingToolCall) -> Result<T, InvalidSubagentToolArguments>
where
    T: DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::Object(
        call.arguments().as_object().clone(),
    ))
    .map_err(|error| InvalidSubagentToolArguments::new(format!("invalid tool input: {error}")))
}

fn task_spec_from_input(
    input: SpawnSubagentTaskInput,
    default_max_model_turns: u32,
) -> Result<SubagentTaskSpec, InvalidSubagentToolArguments> {
    let SpawnSubagentTaskInput {
        task,
        display_name,
        max_model_turns,
        allowed_tools,
        read_scope,
        write_scope,
        forbidden_paths,
        expected_output,
        reasoning_effort,
        plan_client_key,
    } = input;
    let reasoning_effort = reasoning_effort
        .map(|value| merry_llm::ReasoningEffort::new(&value))
        .transpose()
        .map_err(|error| InvalidSubagentToolArguments::new(error.to_string()))?;

    let mut task = SubagentTaskSpec::new(task, max_model_turns.unwrap_or(default_max_model_turns))
        .map_err(InvalidSubagentToolArguments::from)?
        .with_display_name(display_name);
    if let Some(allowed_tools) = allowed_tools {
        task = task.with_allowed_tools(allowed_tools);
    }
    if let Some(read_scope) = read_scope {
        task = task
            .with_read_scope(read_scope)
            .map_err(InvalidSubagentToolArguments::from)?;
    }
    if let Some(write_scope) = write_scope {
        task = task
            .with_write_scope(write_scope)
            .map_err(InvalidSubagentToolArguments::from)?;
    }
    if let Some(forbidden_paths) = forbidden_paths {
        task = task
            .with_forbidden_paths(forbidden_paths)
            .map_err(InvalidSubagentToolArguments::from)?;
    }
    Ok(task
        .with_expected_output(expected_output)
        .with_reasoning_effort(reasoning_effort)
        .with_plan_client_key(plan_client_key))
}

fn succeeded_json_output<T>(tool_name: &str, output: &T) -> ToolExecutionResult
where
    T: Serialize,
{
    let content = serde_json::to_string(output).map_err(|error| {
        ToolExecutionError::infrastructure(format!(
            "failed to serialize {tool_name} output: {error}"
        ))
    })?;
    Ok(ToolExecutionOutcome::succeeded_json(content))
}

fn spawn_json_output(output: &super::super::SpawnSubagentsOutput) -> ToolExecutionResult {
    let content = serde_json::to_string(output).map_err(|error| {
        ToolExecutionError::infrastructure(format!(
            "failed to serialize {SPAWN_SUBAGENTS_TOOL_NAME} output: {error}"
        ))
    })?;

    if output.spawned.is_empty() && !output.rejected.is_empty() {
        return Ok(ToolExecutionOutcome::failed_json(
            content,
            ErrorInfo::new(
                SUBAGENT_SPAWN_REJECTED_CODE,
                "all requested child agents were rejected",
            )
            .expect("static subagent diagnostic code is valid"),
        ));
    }

    Ok(ToolExecutionOutcome::succeeded_json(content))
}

fn infrastructure_error(error: impl std::fmt::Display) -> ToolExecutionError {
    ToolExecutionError::infrastructure(error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InvalidSubagentToolArguments {
    message: String,
}

impl InvalidSubagentToolArguments {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: sanitize_diagnostic_message(message.into()),
        }
    }

    fn message(&self) -> &str {
        &self.message
    }
}

impl From<SubagentError> for InvalidSubagentToolArguments {
    fn from(error: SubagentError) -> Self {
        Self::new(error.to_string())
    }
}

const SUBAGENT_INVALID_ARGUMENTS_CODE: &str = "subagent_invalid_arguments";
const SUBAGENT_SPAWN_REJECTED_CODE: &str = "subagent_spawn_rejected";

fn invalid_subagent_arguments_outcome(
    tool_name: &str,
    error: InvalidSubagentToolArguments,
) -> ToolExecutionOutcome {
    let payload = serde_json::json!({
        "ok": false,
        "tool": tool_name,
        "error": {
            "code": SUBAGENT_INVALID_ARGUMENTS_CODE,
            "message": error.message(),
        },
        "recovery": {
            "input_contract": "Provide arguments matching the subagent tool input schema.",
            "scope_contract": "Paths must be normalized workspace-relative paths.",
            "tool_name_contract": "allowed_tools entries must be exact registered Merry tool names copied from the current tool list. Use run_process, never functions.run_process.",
        }
    });

    ToolExecutionOutcome::failed_json(
        payload.to_string(),
        ErrorInfo::new(SUBAGENT_INVALID_ARGUMENTS_CODE, error.message())
            .expect("static subagent diagnostic code is valid"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ArtifactContent, ChildRuntimeFactory, ChildRuntimeInput, DEFAULT_MAX_MODEL_TURNS, Runtime,
        SubagentConfig, SubagentStatusLabel, ToolExecutionContext, ToolExecutor,
        schema_contract::assert_provider_input_schema_fields_have_descriptions,
    };
    use merry_core::{
        PendingToolCall, SessionId, ToolCallArguments, ToolCallId, ToolCallResultStatus,
    };
    use serde_json::{Value, json};
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex as StdMutex},
    };
    use tokio_util::sync::CancellationToken;

    #[derive(Clone, Default)]
    struct CapturingChildFactory {
        inputs: Arc<StdMutex<Vec<ChildRuntimeInput>>>,
    }

    impl CapturingChildFactory {
        fn inputs(&self) -> Vec<ChildRuntimeInput> {
            self.inputs
                .lock()
                .expect("inputs mutex is not poisoned")
                .clone()
        }
    }

    impl ChildRuntimeFactory for CapturingChildFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
            self.inputs
                .lock()
                .expect("inputs mutex is not poisoned")
                .push(input.clone());

            Runtime::builder(input.session_id)
                .task_anchor(input.task_anchor)
                .build()
        }
    }

    fn manager(factory: Arc<dyn ChildRuntimeFactory>) -> SubagentManager {
        SubagentManager::new(
            SessionId::new("parent").expect("valid session id"),
            SubagentConfig::default(),
            factory,
        )
    }

    fn pending_call(name: &str, arguments: Value) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new("call-1").expect("valid call id"),
            ToolName::new(name).expect("valid tool name"),
            ToolCallArguments::try_from(arguments).expect("object arguments"),
        )
    }

    fn outcome_json(outcome: &crate::ToolExecutionOutcome) -> Value {
        let ArtifactContent::Json { content } = outcome.content() else {
            panic!("expected JSON tool outcome");
        };
        serde_json::from_str(content).expect("outcome contains valid JSON")
    }

    #[test]
    fn spawn_schema_accepts_an_optional_plan_client_key_reference() {
        let specs = subagent_tool_specs().expect("subagent tools build");
        let schema =
            serde_json::to_value(specs[0].input_schema()).expect("spawn schema serializes");
        assert!(schema.to_string().contains("plan_client_key"));
        assert!(!schema.to_string().contains("plan_task"));
        assert!(
            specs[0]
                .description()
                .contains("never pass a runtime node id")
        );
    }

    #[test]
    fn wait_schema_accepts_zero_deadline_as_status_snapshot() {
        let specs = subagent_tool_specs().expect("subagent tools build");
        let schema = serde_json::to_value(specs[1].input_schema()).expect("wait schema serializes");
        assert!(schema.to_string().contains("minimum"));
        assert!(schema.to_string().contains("\"minimum\":0"));
        assert!(specs[1].description().contains("observation deadline"));

        let validator = jsonschema::validator_for(specs[1].input_schema().as_schema().as_value())
            .expect("wait schema compiles");
        assert!(validator.is_valid(&json!({
            "agent_ids": ["agent-1"],
            "timeout_ms": 0
        })));
        assert!(
            specs[1]
                .description()
                .contains(super::WAIT_SEMANTIC_CHECKPOINT_GUIDANCE)
        );
        assert!(
            !specs[1]
                .description()
                .contains("separate UI activity stream")
        );
    }

    #[test]
    fn provider_visible_subagent_schemas_describe_fields_and_match_runtime_bounds() {
        let specs = subagent_tool_specs().expect("subagent tools build");
        for spec in &specs {
            assert_provider_input_schema_fields_have_descriptions(spec);
        }

        let spawn_schema = specs[0].input_schema().as_schema().as_value();
        let spawn_validator =
            jsonschema::validator_for(spawn_schema).expect("spawn schema compiles");
        let valid_task = json!({
            "tasks": [{
                "task": "Review the runtime.",
                "max_model_turns": 1,
                "read_scope": ["crates/merry-runtime"],
                "write_scope": ["tmp/output"]
            }]
        });
        if let Err(error) = spawn_validator.validate(&valid_task) {
            panic!("valid task rejected by schema: {error}");
        }

        let mut zero_turns = valid_task.clone();
        zero_turns["tasks"][0]["max_model_turns"] = json!(0);
        assert!(!spawn_validator.is_valid(&zero_turns));

        let mut oversized_task = valid_task.clone();
        oversized_task["tasks"][0]["task"] = json!("x".repeat(16 * 1024 + 1));
        assert!(!spawn_validator.is_valid(&oversized_task));

        let mut invalid_scope = valid_task;
        invalid_scope["tasks"][0]["read_scope"] = json!(["../outside"]);
        assert!(!spawn_validator.is_valid(&invalid_scope));

        for spec in &specs[1..] {
            let validator = jsonschema::validator_for(spec.input_schema().as_schema().as_value())
                .expect("status operation schema compiles");
            assert!(!validator.is_valid(&json!({ "agent_ids": [] })));
        }
    }

    #[test]
    fn subagent_tool_descriptions_explain_linked_child_and_checkpoint_contract() {
        let specs = subagent_tool_specs().expect("subagent tools build");
        assert!(specs[0].description().contains(CHILD_LINKED_SCOPE_GUIDANCE));
        assert!(
            specs[0]
                .description()
                .contains(LINKED_CHILD_DECOMPOSITION_GUIDANCE)
        );
        assert!(
            specs[1]
                .description()
                .contains("semantic or terminal checkpoints")
        );
        assert!(specs[0].description().contains("next model turn"));
        assert!(
            specs[0]
                .description()
                .contains("does not interrupt the current turn")
        );
        assert!(
            specs[0]
                .description()
                .contains("keep tasks[].task focused on the work")
        );
        assert!(
            specs[0]
                .description()
                .contains("do not repeat scope declarations in task text")
        );
        let schema = serde_json::to_string(specs[0].input_schema()).expect("schema serializes");
        assert!(schema.contains("An empty array makes the child read-only"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_tool_returns_structured_output_and_preserves_task_input() {
        let factory = Arc::new(CapturingChildFactory::default());
        let executor = SpawnSubagentsExecutor::new(manager(factory.clone()));
        let call = pending_call(
            SPAWN_SUBAGENTS_TOOL_NAME,
            json!({
                "max_concurrency": 1,
                "tasks": [{
                    "task": "Review the runtime module.",
                    "display_name": "Runtime review",
                    "max_model_turns": 3,
                    "allowed_tools": ["workspace_read_file"],
                    "read_scope": ["crates/merry-runtime/src"],
                    "write_scope": ["tmp/subagent-output"],
                    "forbidden_paths": ["target", ".git"],
                    "expected_output": "Return a compact findings list.",
                    "reasoning_effort": "low"
                }]
            }),
        );

        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("spawn execution should succeed");
        let output: super::super::SpawnSubagentsOutput =
            serde_json::from_value(outcome_json(&outcome)).expect("spawn output is structured");
        let captured = factory.inputs();

        assert_eq!(output.spawned.len(), 1);
        assert!(output.rejected.is_empty());
        assert_eq!(
            output.spawned[0].display_name.as_deref(),
            Some("Runtime review")
        );
        assert_eq!(
            output.spawned[0].read_scope,
            vec!["crates/merry-runtime/src".to_owned()]
        );
        assert_eq!(
            output.spawned[0].write_scope,
            vec!["tmp/subagent-output".to_owned()]
        );
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].task.display_name(), Some("Runtime review"));
        assert_eq!(captured[0].task.max_model_turns(), 3);
        assert_eq!(
            captured[0].task.allowed_tools(),
            &[ToolName::new("workspace_read_file").expect("valid tool name")]
        );
        assert_eq!(
            captured[0].allowed_tools,
            vec![ToolName::new("workspace_read_file").expect("valid tool name")]
        );
        assert_eq!(
            captured[0].task.read_scope(),
            &[PathBuf::from("crates/merry-runtime/src")]
        );
        assert_eq!(
            captured[0].workspace_scope.read_scope(),
            &[PathBuf::from("crates/merry-runtime/src")]
        );
        assert_eq!(
            captured[0].task.write_scope(),
            &[PathBuf::from("tmp/subagent-output")]
        );
        assert_eq!(
            captured[0].workspace_scope.write_scope(),
            &[PathBuf::from("tmp/subagent-output")]
        );
        assert_eq!(
            captured[0].task.forbidden_paths(),
            &[PathBuf::from(".git"), PathBuf::from("target")]
        );
        assert_eq!(
            captured[0].workspace_scope.forbidden_paths(),
            &[PathBuf::from(".git"), PathBuf::from("target")]
        );
        assert_eq!(
            captured[0].task.expected_output(),
            Some("Return a compact findings list.")
        );
        assert_eq!(
            captured[0]
                .task
                .reasoning_effort()
                .map(|effort| effort.as_str()),
            Some("low")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_tool_returns_failed_outcome_when_all_tasks_are_rejected() {
        let factory = Arc::new(CapturingChildFactory::default());
        let manager = SubagentManager::runtime_controlled(
            SessionId::new("spawn-all-rejected").expect("valid session id"),
            SubagentConfig::default(),
            factory.clone(),
            false,
        );
        let executor = SpawnSubagentsExecutor::new(manager);
        let call = pending_call(
            SPAWN_SUBAGENTS_TOOL_NAME,
            json!({"tasks": [{"task": "This must not start."}]}),
        );

        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("rejected spawn should resolve as a failed tool outcome");

        assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
        assert_eq!(
            outcome
                .diagnostic()
                .expect("rejected spawn should include a diagnostic")
                .code(),
            SUBAGENT_SPAWN_REJECTED_CODE
        );
        let payload = outcome_json(&outcome);
        assert!(
            payload["spawned"]
                .as_array()
                .expect("spawned array")
                .is_empty()
        );
        assert_eq!(payload["rejected"].as_array().map(Vec::len), Some(1));
        assert!(factory.inputs().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_tool_defaults_max_model_turns_when_omitted() {
        let factory = Arc::new(CapturingChildFactory::default());
        let executor = SpawnSubagentsExecutor::new(manager(factory.clone()));
        let call = pending_call(
            SPAWN_SUBAGENTS_TOOL_NAME,
            json!({
                "tasks": [{
                    "task": "Use the default model-turn limit."
                }]
            }),
        );

        executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("spawn execution should succeed");
        let captured = factory.inputs();

        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].task.max_model_turns(), DEFAULT_MAX_MODEL_TURNS);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_tool_uses_manager_configured_default_max_model_turns() {
        let factory = Arc::new(CapturingChildFactory::default());
        let config = SubagentConfig::default()
            .with_max_model_turns(96)
            .expect("positive child model-turn limit");
        let manager = SubagentManager::new(
            SessionId::new("configured-default").expect("valid session id"),
            config,
            factory.clone(),
        );
        let executor = SpawnSubagentsExecutor::new(manager);
        let call = pending_call(
            SPAWN_SUBAGENTS_TOOL_NAME,
            json!({
                "tasks": [{"task": "Use the configured model-turn limit."}]
            }),
        );

        executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("spawn execution should succeed");

        assert_eq!(factory.inputs()[0].task.max_model_turns(), 96);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_tool_uses_random_child_session_ids() {
        let factory = Arc::new(CapturingChildFactory::default());
        let executor = SpawnSubagentsExecutor::new(manager(factory.clone()));
        let call = pending_call(
            SPAWN_SUBAGENTS_TOOL_NAME,
            json!({
                "max_concurrency": 2,
                "tasks": [
                    { "task": "Inspect one file." },
                    { "task": "Inspect another file." }
                ]
            }),
        );

        executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("spawn execution should succeed");
        let captured = factory.inputs();

        assert_eq!(captured.len(), 2);
        let first = captured[0].session_id.as_str();
        let second = captured[1].session_id.as_str();
        assert_ne!(first, second);
        assert_eq!(first.len(), 36);
        assert_eq!(second.len(), 36);
        assert!(!first.starts_with("parent-agent-"));
        assert!(!second.starts_with("parent-agent-"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_tool_returns_status_output() {
        let manager = manager(Arc::new(CapturingChildFactory::default()));
        let spawn = manager
            .spawn(
                vec![SubagentTaskSpec::new("Stay queued for wait.", 2).expect("valid task")],
                Some(0),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let executor = WaitSubagentsExecutor::new(manager);
        let call = pending_call(
            WAIT_SUBAGENTS_TOOL_NAME,
            json!({
                "agent_ids": [spawn.spawned[0].agent_id],
                "timeout_ms": 0
            }),
        );

        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("wait execution should succeed");
        let output: super::super::WaitSubagentsOutput =
            serde_json::from_value(outcome_json(&outcome)).expect("wait output is structured");

        assert_eq!(output.agents.len(), 1);
        assert_eq!(output.agents[0].status, SubagentStatusLabel::Queued);
        assert_eq!(output.agents[0].summary, "child queued");
        let payload = outcome_json(&outcome);
        assert_eq!(payload["timed_out"], true);
        assert_eq!(payload["terminal"], false);
        assert_eq!(
            payload["pending_agent_ids"].as_array().map(Vec::len),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_tool_returns_cancelled_status_output() {
        let manager = manager(Arc::new(CapturingChildFactory::default()));
        let spawn = manager
            .spawn(
                vec![SubagentTaskSpec::new("Cancel queued child.", 2).expect("valid task")],
                Some(0),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let executor = CancelSubagentsExecutor::new(manager);
        let call = pending_call(
            CANCEL_SUBAGENTS_TOOL_NAME,
            json!({
                "agent_ids": [spawn.spawned[0].agent_id]
            }),
        );

        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("cancel execution should succeed");
        let output: super::super::WaitSubagentsOutput =
            serde_json::from_value(outcome_json(&outcome)).expect("cancel output is structured");

        assert_eq!(output.agents.len(), 1);
        assert_eq!(output.agents[0].status, SubagentStatusLabel::Cancelled);
        assert_eq!(output.agents[0].summary, "child cancelled by parent");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_tool_invalid_task_or_path_returns_failed_outcome() {
        let executor =
            SpawnSubagentsExecutor::new(manager(Arc::new(CapturingChildFactory::default())));

        for arguments in [
            json!({ "tasks": [{ "task": " " }] }),
            json!({ "tasks": [{ "task": "Bad path.", "read_scope": ["../secret"] }] }),
            json!({ "tasks": [{ "task": "Bad control path.", "read_scope": ["bad\npath"] }] }),
            json!({ "tasks": [{ "task": "Bad effort.", "reasoning_effort": "bad\neffort" }] }),
        ] {
            let call = pending_call(SPAWN_SUBAGENTS_TOOL_NAME, arguments);
            let outcome = executor
                .execute(call, ToolExecutionContext::default())
                .await
                .expect("invalid input should resolve as failed tool outcome");
            let diagnostic = outcome
                .diagnostic()
                .expect("failed subagent arguments should include diagnostic");
            let payload = outcome_json(&outcome);

            assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
            assert_eq!(diagnostic.code(), SUBAGENT_INVALID_ARGUMENTS_CODE);
            assert_eq!(payload["ok"], false);
            assert_eq!(payload["error"]["code"], SUBAGENT_INVALID_ARGUMENTS_CODE);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_tool_recovery_rejects_provider_namespaced_allowed_tools() {
        let executor =
            SpawnSubagentsExecutor::new(manager(Arc::new(CapturingChildFactory::default())));
        let call = pending_call(
            SPAWN_SUBAGENTS_TOOL_NAME,
            json!({
                "tasks": [{
                    "task": "Inspect the runtime.",
                    "allowed_tools": ["functions.run_process"]
                }]
            }),
        );

        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("invalid input should resolve as a failed tool outcome");
        let payload = outcome_json(&outcome);
        let recovery = payload["recovery"]["tool_name_contract"]
            .as_str()
            .expect("tool-name recovery should be present");

        assert!(recovery.contains("exact registered Merry tool names"));
        assert!(recovery.contains("run_process"));
        assert!(recovery.contains("functions.run_process"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_input_shape_errors_return_failed_outcome() {
        let executor =
            WaitSubagentsExecutor::new(manager(Arc::new(CapturingChildFactory::default())));
        let call = pending_call(
            WAIT_SUBAGENTS_TOOL_NAME,
            json!({
                "agent_ids": [],
                "unexpected": true
            }),
        );

        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("invalid provider-visible input should resolve as failed outcome");
        let diagnostic = outcome
            .diagnostic()
            .expect("failed input should include diagnostic");
        let payload = outcome_json(&outcome);

        assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
        assert_eq!(diagnostic.code(), SUBAGENT_INVALID_ARGUMENTS_CODE);
        assert_eq!(payload["tool"], WAIT_SUBAGENTS_TOOL_NAME);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_tool_without_timeout_honors_cancellation() {
        let manager = manager(Arc::new(CapturingChildFactory::default()));
        let spawn = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Stay queued until cancellation.", 2)
                        .expect("valid task"),
                ],
                Some(0),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let executor = WaitSubagentsExecutor::new(manager);
        let call = pending_call(
            WAIT_SUBAGENTS_TOOL_NAME,
            json!({
                "agent_ids": [spawn.spawned[0].agent_id],
                "mode": "all"
            }),
        );
        let token = CancellationToken::new();
        token.cancel();

        let error = executor
            .execute(call, ToolExecutionContext::new(token))
            .await
            .expect_err("pre-cancelled wait should not resolve");

        assert!(matches!(error, ToolExecutionError::Cancelled));
    }

    #[test]
    fn registered_subagent_tools_are_named_with_control_policy() {
        let tools = subagent_registered_tools(manager(Arc::new(CapturingChildFactory::default())))
            .expect("registered subagent tools should build");

        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].spec().name().as_str(), SPAWN_SUBAGENTS_TOOL_NAME);
        assert_eq!(tools[1].spec().name().as_str(), WAIT_SUBAGENTS_TOOL_NAME);
        assert_eq!(tools[2].spec().name().as_str(), CANCEL_SUBAGENTS_TOOL_NAME);
        assert_eq!(tools[0].action_kind(), ToolActionKind::RuntimeControl);
        assert_eq!(tools[1].action_kind(), ToolActionKind::ReadOnly);
        assert_eq!(tools[2].action_kind(), ToolActionKind::RuntimeControl);
    }

    #[test]
    fn registered_subagent_schema_uses_manager_configured_model_turn_default() {
        let config = SubagentConfig::default()
            .with_max_model_turns(96)
            .expect("positive child model-turn limit");
        let manager = SubagentManager::new(
            SessionId::new("configured-schema").expect("valid session id"),
            config,
            Arc::new(CapturingChildFactory::default()),
        );
        let tools = subagent_registered_tools(manager).expect("registered tools should build");
        let schema =
            serde_json::to_value(tools[0].spec().input_schema()).expect("spawn schema serializes");

        assert_eq!(find_max_model_turns_default(&schema), Some(96));
        assert!(
            tools[0]
                .spec()
                .description()
                .contains("runtime default (96")
        );
    }

    fn find_max_model_turns_default(value: &Value) -> Option<u64> {
        let object = value.as_object()?;
        if let Some(default) = object
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|properties| properties.get("max_model_turns"))
            .and_then(Value::as_object)
            .and_then(|field| field.get("default"))
            .and_then(Value::as_u64)
        {
            return Some(default);
        }
        object.values().find_map(find_max_model_turns_default)
    }
}
