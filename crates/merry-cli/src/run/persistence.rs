//! Resume-safe headless persistence and deterministic failure precedence.

use crate::{
    cli_error::{CliError, unexpected},
    run::{
        RunExitStatus, RunSession,
        output::{RunOutput, SettledRun, settle_agent_loop_output},
    },
    tui::session_list::{TuiSessionMetadata, TuiSessionStore, now_unix_ms},
};
use merry_core::SessionId;
use merry_runtime::{AgentLoopConfig, FileSessionStore, Runtime, StepContext, StepInput};
use std::future::Future;
use tokio::io::AsyncWrite;

/// Recorded against every tool call a settled run never produced a result for.
pub(super) const ABANDONED_TOOL_CALL_REASON: &str =
    "the headless run settled before this tool call produced a result";

/// Settles the runtime and permission reviewer before attempting persistence.
///
/// This is the production headless-run orchestration boundary. Presentation
/// failures stop the event producer at the output boundary; persistence is
/// then attempted after reviewer shutdown. Final error selection happens only
/// after that save attempt.
pub(super) struct HeadlessRunPersistence<'a> {
    pub(super) loop_config: AgentLoopConfig,
    pub(super) context: StepContext,
    pub(super) events_jsonl: bool,
    pub(super) session_store: FileSessionStore,
    pub(super) session_id: &'a SessionId,
    pub(super) metadata: TuiSessionMetadata,
}

pub(super) async fn run_agent_loop_with_persistence<W, F>(
    runtime: &Runtime,
    input: StepInput,
    writer: W,
    review_result: F,
    persistence: HeadlessRunPersistence<'_>,
) -> Result<RunExitStatus, CliError>
where
    W: AsyncWrite + Unpin,
    F: Future<Output = Result<(), CliError>>,
{
    let settled = settle_agent_loop_output(
        runtime,
        input,
        persistence.loop_config,
        persistence.context,
        writer,
        RunOutput::new(persistence.events_jsonl),
    )
    .await;
    let review_result = review_result.await;
    finish_settled_run(
        runtime,
        persistence.session_store,
        persistence.session_id,
        persistence.metadata,
        settled,
        review_result,
    )
    .await
}

/// Persists a settled run before selecting its final result.
///
/// A persistence failure takes precedence over runtime, stdout, or reviewer
/// failures so the CLI cannot report only an earlier failure when durable state
/// was not written. When persistence succeeds, errors are returned in runtime,
/// presentation, then reviewer order.
pub(super) async fn finish_settled_run(
    runtime: &Runtime,
    session_store: FileSessionStore,
    session_id: &SessionId,
    metadata: TuiSessionMetadata,
    settled: SettledRun,
    review_result: Result<(), CliError>,
) -> Result<RunExitStatus, CliError> {
    let SettledRun {
        runtime_result,
        presentation_result,
    } = settled;
    persist_settled_session(runtime, session_store, session_id, metadata).await?;
    let status = runtime_result?;
    presentation_result?;
    review_result?;
    Ok(status)
}

/// Records a result for every tool call the run left pending, then saves.
///
/// A run that fails mid-step settles with its tool call still unresolved, and
/// the session store refuses that state because it cannot be resumed. Giving
/// those calls a durable failed result is what makes a failed run's partial
/// transcript resumable on the shipped path, rather than only under a reviewer
/// that happens to submit results of its own.
pub(super) async fn persist_settled_session(
    runtime: &Runtime,
    session_store: FileSessionStore,
    session_id: &SessionId,
    metadata: TuiSessionMetadata,
) -> Result<(), CliError> {
    runtime
        .abandon_pending_tool_calls(ABANDONED_TOOL_CALL_REASON)
        .await
        .map_err(|error| {
            unexpected(format!(
                "run finished but session {session_id} could not be made resume-safe: {error}"
            ))
        })?;
    runtime
        .save_session_to(session_store.clone())
        .await
        .map_err(|error| {
            unexpected(format!(
                "run finished but session {session_id} could not be saved: {error}"
            ))
        })?;
    write_headless_session_metadata(&session_store, metadata).await
}

pub(super) async fn headless_session_metadata(
    store: &FileSessionStore,
    session: &RunSession,
    workspace_root: &std::path::Path,
) -> Result<TuiSessionMetadata, CliError> {
    let tui_store = TuiSessionStore::new(store.sessions_dir().to_path_buf());
    let session_id = session.id().clone();
    let existing = tokio::task::spawn_blocking({
        let tui_store = tui_store.clone();
        let session_id = session_id.clone();
        move || tui_store.read_metadata(&session_id)
    })
    .await
    .map_err(unexpected)?
    .map_err(unexpected)?;
    let mut metadata = existing.unwrap_or_else(|| {
        TuiSessionMetadata::new(
            session_id.clone(),
            workspace_root.to_path_buf(),
            now_unix_ms(),
        )
    });
    metadata.workspace_root = workspace_root.to_path_buf();
    if matches!(session, RunSession::New(_)) {
        metadata.headless = true;
        if metadata.title.is_none() {
            metadata.title = Some("Headless run".to_owned());
        }
    }
    metadata.mark_active(now_unix_ms());
    Ok(metadata)
}

pub(super) async fn write_headless_session_metadata(
    session_store: &FileSessionStore,
    metadata: TuiSessionMetadata,
) -> Result<(), CliError> {
    let store = TuiSessionStore::new(session_store.sessions_dir().to_path_buf());
    tokio::task::spawn_blocking(move || store.write_metadata(&metadata))
        .await
        .map_err(unexpected)?
        .map_err(unexpected)
}
