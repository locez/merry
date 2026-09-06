//! One owned agent-stream lifecycle with human and JSONL presentation adapters.

use crate::{
    cli_error::{CliError, stdout_error, unexpected},
    run::RunExitStatus,
};
use human::{write_agent_loop_summary_to, write_human_progress_event};
use jsonl::{write_agent_loop_result, write_public_runtime_event};
use merry_core::RuntimeEvent;
use merry_runtime::{AgentLoopConfig, AgentLoopResult, Runtime, StepContext, StepInput};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

mod human;

mod jsonl;

/// Runtime settlement plus the first failure encountered while presenting it.
///
/// Output failure cancels and awaits the producer. Persistence selects the
/// reported failure only after saving the settled runtime state.
#[derive(Debug)]
pub(super) struct SettledRun {
    pub(super) runtime_result: Result<RunExitStatus, CliError>,
    pub(super) presentation_result: Result<(), CliError>,
}

#[cfg(test)]
impl SettledRun {
    pub(super) fn into_output_result(self) -> Result<RunExitStatus, CliError> {
        let status = self.runtime_result?;
        self.presentation_result?;
        Ok(status)
    }
}

#[cfg(test)]
pub(super) async fn write_agent_loop_output<W>(
    runtime: &Runtime,
    input: StepInput,
    config: AgentLoopConfig,
    context: StepContext,
    writer: W,
) -> Result<RunExitStatus, CliError>
where
    W: AsyncWrite + Unpin,
{
    settle_agent_loop_output(
        runtime,
        input,
        config,
        context,
        writer,
        RunOutput::new(false),
    )
    .await
    .into_output_result()
}

pub(super) enum RunOutput {
    Human { pending_commentary: Option<String> },
    Jsonl,
}

impl RunOutput {
    pub(super) fn new(events_jsonl: bool) -> Self {
        if events_jsonl {
            Self::Jsonl
        } else {
            Self::Human {
                pending_commentary: None,
            }
        }
    }

    pub(super) async fn write_event<W>(
        &mut self,
        event: &RuntimeEvent,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        match self {
            Self::Human { pending_commentary } => {
                write_human_progress_event(event, pending_commentary, writer).await
            }
            Self::Jsonl => {
                write_public_runtime_event(event, writer).await?;
                writer.flush().await.map_err(stdout_error)
            }
        }
    }

    pub(super) async fn write_result<W>(
        &self,
        result: &AgentLoopResult,
        writer: &mut W,
    ) -> Result<(), CliError>
    where
        W: AsyncWrite + Unpin,
    {
        match self {
            Self::Human { .. } => write_agent_loop_summary_to(result, writer).await?,
            Self::Jsonl => write_agent_loop_result(result, writer).await?,
        }
        writer.flush().await.map_err(stdout_error)
    }
}

pub(super) async fn settle_agent_loop_output<W>(
    runtime: &Runtime,
    input: StepInput,
    config: AgentLoopConfig,
    context: StepContext,
    writer: W,
    mut output: RunOutput,
) -> SettledRun
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    let mut stream = match runtime.run_agent_loop_stream(input, context, config) {
        Ok(stream) => stream,
        Err(error) => {
            return SettledRun {
                runtime_result: Err(unexpected(error)),
                presentation_result: Ok(()),
            };
        }
    };
    loop {
        let event = match stream.next_message().await {
            Ok(Some(merry_runtime::AgentRunMessage::Event(event))) => event,
            Ok(Some(merry_runtime::AgentRunMessage::ToolInvocations { batch })) => {
                stream.cancel_and_wait().await;
                return SettledRun {
                    runtime_result: Err(unexpected(format!(
                        "CLI received {} host tool invocations, but this path requires runtime-owned tools",
                        batch.calls().len()
                    ))),
                    presentation_result: Ok(()),
                };
            }
            Ok(Some(_)) => {
                stream.cancel_and_wait().await;
                return SettledRun {
                    runtime_result: Err(unexpected(
                        "runtime emitted an unsupported agent run message",
                    )),
                    presentation_result: Ok(()),
                };
            }
            Ok(None) => break,
            Err(error) => {
                stream.cancel_and_wait().await;
                return SettledRun {
                    runtime_result: Err(unexpected(error)),
                    presentation_result: Ok(()),
                };
            }
        };
        if let Err(error) = output.write_event(&event, &mut writer).await {
            stream.cancel_and_wait().await;
            return SettledRun {
                runtime_result: Ok(RunExitStatus::Incomplete),
                presentation_result: Err(error),
            };
        }
    }
    match stream.result().await.map_err(unexpected) {
        Ok(result) => SettledRun {
            runtime_result: Ok(RunExitStatus::from_agent_loop_result(&result)),
            presentation_result: output.write_result(&result, &mut writer).await,
        },
        Err(error) => SettledRun {
            runtime_result: Err(error),
            presentation_result: Ok(()),
        },
    }
}

#[cfg(test)]
pub(super) async fn write_agent_loop_jsonl_output<W>(
    runtime: &Runtime,
    input: StepInput,
    config: AgentLoopConfig,
    context: StepContext,
    writer: W,
) -> Result<RunExitStatus, CliError>
where
    W: AsyncWrite + Unpin,
{
    settle_agent_loop_output(
        runtime,
        input,
        config,
        context,
        writer,
        RunOutput::new(true),
    )
    .await
    .into_output_result()
}
