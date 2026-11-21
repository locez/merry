//! Model tools for processes retained by `run_process` with `background: true`.

use crate::{
    BackgroundProcessControl, BackgroundProcessId, BackgroundProcessLookupError,
    BackgroundProcessOutput, BackgroundProcessStop, BackgroundProcessSummary,
    ProcessCommandToolError, RegisteredTool, ToolActionKind, ToolExecutionContext,
    ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use merry_core::{ErrorInfo, PendingToolCall};
use merry_tools_macros::tool;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

const INVALID_ARGUMENTS: &str = "background_process_invalid_arguments";
const NOT_FOUND: &str = "background_process_not_found";

#[tool(
    crate = "crate",
    name = "read_background_process",
    description = "Read a background process retained by run_process. Omit process_id to list id, state, and command. Provide the id from that result to read later stdout and stderr."
)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadBackgroundProcessInput {
    #[schemars(
        description = "Id reported by a background run_process result. Omit it to list retained processes."
    )]
    #[serde(default)]
    process_id: Option<String>,
}

#[tool(
    crate = "crate",
    name = "stop_background_process",
    description = "Stop one background process group by the id from a background run_process result. A process that has already exited or been killed reports that state."
)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StopBackgroundProcessInput {
    #[schemars(
        description = "Id reported by a background run_process result.",
        length(min = 1, max = 64)
    )]
    process_id: String,
}

/// Registers the read-only background process tool.
pub fn read_background_process_tool(
    control: Arc<dyn BackgroundProcessControl>,
) -> Result<RegisteredTool, ProcessCommandToolError> {
    let spec =
        ReadBackgroundProcessInput::tool_spec().map_err(|error| ProcessCommandToolError::Core {
            source: error.into(),
        })?;
    Ok(
        RegisteredTool::read_only(spec, Arc::new(ReadBackgroundProcessExecutor { control }))
            .with_parallel_safe_execution(),
    )
}

/// Registers the tool that stops one retained background process group.
pub fn stop_background_process_tool(
    control: Arc<dyn BackgroundProcessControl>,
) -> Result<RegisteredTool, ProcessCommandToolError> {
    let spec =
        StopBackgroundProcessInput::tool_spec().map_err(|error| ProcessCommandToolError::Core {
            source: error.into(),
        })?;
    Ok(RegisteredTool::new(
        spec,
        Arc::new(StopBackgroundProcessExecutor { control }),
        ToolActionKind::RuntimeControl,
    )
    .with_parallel_safe_execution())
}

struct ReadBackgroundProcessExecutor {
    control: Arc<dyn BackgroundProcessControl>,
}

impl ToolExecutor for ReadBackgroundProcessExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input = match call
                .arguments()
                .deserialize_as::<ReadBackgroundProcessInput>()
            {
                Ok(input) => input,
                Err(error) => return Ok(invalid_arguments(error.to_string())),
            };
            let Some(process_id) = input
                .process_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                return Ok(ToolExecutionOutcome::succeeded_text(format_list(
                    &self.control.list(),
                )));
            };
            let id = match BackgroundProcessId::new(process_id) {
                Ok(id) => id,
                Err(message) => return Ok(invalid_arguments(message)),
            };
            match self.control.read(&id) {
                Ok(output) => Ok(ToolExecutionOutcome::succeeded_text(format_output(&output))),
                Err(error) => Ok(not_found(&error)),
            }
        })
    }
}

struct StopBackgroundProcessExecutor {
    control: Arc<dyn BackgroundProcessControl>,
}

impl ToolExecutor for StopBackgroundProcessExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input = match call
                .arguments()
                .deserialize_as::<StopBackgroundProcessInput>()
            {
                Ok(input) => input,
                Err(error) => return Ok(invalid_arguments(error.to_string())),
            };
            let id = match BackgroundProcessId::new(input.process_id.trim()) {
                Ok(id) => id,
                Err(message) => return Ok(invalid_arguments(message)),
            };
            match self.control.stop(&id) {
                Ok(stopped) => Ok(ToolExecutionOutcome::succeeded_text(format_stop(&stopped))),
                Err(error) => Ok(not_found(&error)),
            }
        })
    }
}

fn format_list(processes: &[BackgroundProcessSummary]) -> String {
    if processes.is_empty() {
        return "No background processes.".to_owned();
    }
    processes
        .iter()
        .map(|process| {
            format!(
                "{} {} {}",
                process.id(),
                process.state().label(),
                process.command()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_output(output: &BackgroundProcessOutput) -> String {
    let summary = output.summary();
    let stdout_label = stream_label("stdout", output.stdout_truncated());
    let stderr_label = stream_label("stderr", output.stderr_truncated());
    format!(
        "{} {}\ncommand: {}\n{stdout_label}:\n{}\n{stderr_label}:\n{}",
        summary.id(),
        summary.state().label(),
        summary.command(),
        output.stdout(),
        output.stderr()
    )
}

fn format_stop(stopped: &BackgroundProcessStop) -> String {
    let summary = stopped.summary();
    if stopped.changed() {
        format!("{} killed", summary.id())
    } else {
        format!("{} already {}", summary.id(), summary.state().label())
    }
}

fn stream_label(name: &str, truncated: bool) -> String {
    if truncated {
        format!("{name} (truncated)")
    } else {
        name.to_owned()
    }
}

fn invalid_arguments(message: String) -> ToolExecutionOutcome {
    failed_outcome(INVALID_ARGUMENTS, message)
}

fn not_found(error: &BackgroundProcessLookupError) -> ToolExecutionOutcome {
    failed_outcome(NOT_FOUND, error.message())
}

fn failed_outcome(code: &str, message: String) -> ToolExecutionOutcome {
    ToolExecutionOutcome::failed_text(
        message.clone(),
        ErrorInfo::new(code, &message).expect("background process diagnostic is valid"),
    )
}

#[cfg(test)]
mod tests {
    use super::{ReadBackgroundProcessExecutor, StopBackgroundProcessExecutor, format_list};
    use crate::{
        BackgroundProcessControl, BackgroundProcessId, BackgroundProcessLookupError,
        BackgroundProcessOutput, BackgroundProcessState, BackgroundProcessStop,
        BackgroundProcessSummary, ToolExecutionContext, ToolExecutor,
    };
    use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    struct FakeControl {
        processes: Mutex<Vec<BackgroundProcessSummary>>,
    }

    impl FakeControl {
        fn running() -> Self {
            Self {
                processes: Mutex::new(vec![BackgroundProcessSummary::new(
                    BackgroundProcessId::new("bg-1").expect("id"),
                    "sleep 30",
                    None,
                    BackgroundProcessState::Running { pid: 42 },
                )]),
            }
        }
    }

    impl BackgroundProcessControl for FakeControl {
        fn list(&self) -> Vec<BackgroundProcessSummary> {
            self.processes.lock().expect("process list").clone()
        }

        fn read(
            &self,
            id: &BackgroundProcessId,
        ) -> Result<BackgroundProcessOutput, BackgroundProcessLookupError> {
            let processes = self.list();
            let summary = processes
                .iter()
                .find(|process| process.id() == id)
                .cloned()
                .ok_or_else(|| {
                    BackgroundProcessLookupError::new(
                        id.clone(),
                        processes
                            .iter()
                            .map(|process| process.id().clone())
                            .collect(),
                    )
                })?;
            Ok(BackgroundProcessOutput::new(
                summary, "ready\n", "", false, false,
            ))
        }

        fn stop(
            &self,
            id: &BackgroundProcessId,
        ) -> Result<BackgroundProcessStop, BackgroundProcessLookupError> {
            let mut processes = self.processes.lock().expect("process list");
            let Some(process) = processes.iter_mut().find(|process| process.id() == id) else {
                let known = processes
                    .iter()
                    .map(|process| process.id().clone())
                    .collect();
                return Err(BackgroundProcessLookupError::new(id.clone(), known));
            };
            let changed = matches!(process.state(), BackgroundProcessState::Running { .. });
            if changed {
                let id = process.id().clone();
                let command = process.command().to_owned();
                let cwd = process.cwd().map(str::to_owned);
                *process =
                    BackgroundProcessSummary::new(id, command, cwd, BackgroundProcessState::Killed);
            }
            Ok(BackgroundProcessStop::new(process.clone(), changed))
        }
    }

    fn call(name: &str, arguments: serde_json::Value) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new("call-background").expect("call id"),
            ToolName::new(name).expect("tool name"),
            ToolCallArguments::try_from(arguments).expect("arguments"),
        )
    }

    fn text(outcome: &crate::ToolExecutionOutcome) -> String {
        outcome.content().as_str().to_owned()
    }

    #[tokio::test]
    async fn read_lists_when_the_id_is_omitted_and_returns_output_for_an_id() {
        let control = Arc::new(FakeControl::running());
        let executor = ReadBackgroundProcessExecutor {
            control: control.clone(),
        };
        let listed = executor
            .execute(
                call("read_background_process", json!({})),
                ToolExecutionContext::default(),
            )
            .await
            .expect("list");
        assert_eq!(text(&listed), format_list(&control.list()));
        assert!(text(&listed).contains("bg-1 running pid 42 sleep 30"));

        let read = executor
            .execute(
                call("read_background_process", json!({"process_id": "bg-1"})),
                ToolExecutionContext::default(),
            )
            .await
            .expect("read");
        let body = text(&read);
        assert!(body.contains("bg-1 running pid 42"));
        assert!(body.contains("stdout:\nready\n"));
    }

    #[tokio::test]
    async fn stop_kills_a_running_process_and_names_an_unknown_id() {
        let executor = StopBackgroundProcessExecutor {
            control: Arc::new(FakeControl::running()),
        };
        let stopped = executor
            .execute(
                call("stop_background_process", json!({"process_id": "bg-1"})),
                ToolExecutionContext::default(),
            )
            .await
            .expect("stop");
        assert_eq!(text(&stopped), "bg-1 killed");

        let missing = executor
            .execute(
                call("stop_background_process", json!({"process_id": "bg-9"})),
                ToolExecutionContext::default(),
            )
            .await
            .expect("missing id is a tool result");
        assert!(text(&missing).contains("No background process `bg-9`"));
        assert!(text(&missing).contains("bg-1"));
    }

    #[test]
    fn empty_list_says_nothing_is_retained() {
        assert_eq!(format_list(&[]), "No background processes.");
    }
}
