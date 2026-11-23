//! Transactional installation of a prepared compaction.

use super::RuntimeInner;
use crate::{
    CitationCompactionInput, RuntimeError,
    compaction::{ArchiveOnlyCompactionInput, CompactionOutcome},
    events::ActiveStepPermit,
    session::{JournalCursor, PreparedCompactionInstall, SessionState},
    session_store::StagedSessionBundle,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
pub(in crate::runtime) async fn install_summary_compaction_transactionally(
    inner: Arc<RuntimeInner>,
    input: CitationCompactionInput,
    summary: &str,
    token: CancellationToken,
    active_permit: ActiveStepPermit,
) -> Result<CompactionOutcome, RuntimeError> {
    let summary = summary.to_owned();
    let retention = inner.history_retention;
    let outcome = install_compaction_transaction(inner, &token, active_permit, move |session| {
        session.prepare_summary_compaction_install_with_retention(input, &summary, &retention)
    })
    .await?;
    Ok(outcome.expect("prepared summary replacement must carry an outcome"))
}

pub(in crate::runtime) async fn install_archive_only_compaction_transactionally(
    inner: Arc<RuntimeInner>,
    input: ArchiveOnlyCompactionInput,
    token: CancellationToken,
    active_permit: ActiveStepPermit,
) -> Result<(), RuntimeError> {
    let outcome = install_compaction_transaction(inner, &token, active_permit, move |session| {
        session.prepare_archive_only_compaction_install(input)
    })
    .await?;
    debug_assert!(
        outcome.is_none(),
        "prepared archive-only install must not carry an outcome"
    );
    Ok(())
}

async fn install_compaction_transaction(
    inner: Arc<RuntimeInner>,
    token: &CancellationToken,
    active_permit: ActiveStepPermit,
    prepare: impl FnOnce(&SessionState) -> Result<PreparedCompactionInstall, RuntimeError>,
) -> Result<Option<CompactionOutcome>, RuntimeError> {
    let store = inner.session_store.clone();
    let mut session = tokio::select! {
        biased;
        () = token.cancelled() => return Err(compaction_cancelled_before_install()),
        session = inner.session.lock() => session,
    };
    if token.is_cancelled() {
        return Err(compaction_cancelled_before_install());
    }

    let prepared = prepare(&session)?;
    let bundle = session.persistable_bundle_with_compaction(&prepared, &inner.history_retention)?;
    let cursor = bundle.cursor.clone();
    let rotated = bundle.rotation.is_some();
    let Some(store) = store else {
        if token.is_cancelled() {
            return Err(compaction_cancelled_before_install());
        }
        session.revalidate_prepared_compaction_install(&prepared)?;
        if token.is_cancelled() {
            return Err(compaction_cancelled_before_install());
        }
        return Ok(session.commit_prepared_compaction_install(prepared));
    };
    drop(session);

    let token = token.clone();
    let trace_token = token.clone();
    let session_id = inner.session_id.clone();
    let commit_task = tokio::spawn(async move {
        let result = async {
            if token.is_cancelled() {
                return Err(compaction_cancelled_before_install());
            }
            let staged = store.stage_bundle(bundle).await?;
            complete_staged_compaction(
                inner,
                staged,
                prepared,
                cursor,
                rotated,
                token,
                active_permit,
            )
            .await
        }
        .await;
        if let Err(error) = &result {
            if matches!(error, RuntimeError::SessionStore { .. }) || !trace_token.is_cancelled() {
                tracing::warn!(
                    session_id = %session_id,
                    error = %error,
                    "compaction transaction task failed"
                );
            } else {
                tracing::debug!(
                    session_id = %session_id,
                    error = %error,
                    "compaction transaction task cancelled"
                );
            }
        }
        result
    });
    commit_task
        .await
        .map_err(|error| RuntimeError::CompactionModelStream {
            message: format!("compaction commit task failed: {error}"),
        })?
}

async fn complete_staged_compaction(
    inner: Arc<RuntimeInner>,
    staged: StagedSessionBundle,
    prepared: PreparedCompactionInstall,
    cursor: JournalCursor,
    rotated: bool,
    token: CancellationToken,
    _active_permit: ActiveStepPermit,
) -> Result<Option<CompactionOutcome>, RuntimeError> {
    if token.is_cancelled() {
        return Err(discard_staged_with_error(staged, compaction_cancelled_before_install()).await);
    }

    if let Err(error) = revalidate_staged_compaction(&inner, &token, &prepared).await {
        return Err(discard_staged_with_error(staged, error).await);
    }

    if token.is_cancelled() {
        return Err(discard_staged_with_error(staged, compaction_cancelled_before_install()).await);
    }
    let commit = staged.commit().await?;
    let outcome = {
        let mut session = inner.session.lock().await;
        let outcome = session.commit_prepared_compaction_install(prepared);
        if rotated {
            session.replace_journal_cursor(cursor);
        } else {
            session.adopt_journal_cursor(cursor);
        }
        outcome
    };
    commit.require_durable()?;
    Ok(outcome)
}

async fn revalidate_staged_compaction(
    inner: &RuntimeInner,
    token: &CancellationToken,
    prepared: &PreparedCompactionInstall,
) -> Result<(), RuntimeError> {
    let session = tokio::select! {
        biased;
        () = token.cancelled() => return Err(compaction_cancelled_before_install()),
        session = inner.session.lock() => session,
    };
    if token.is_cancelled() {
        return Err(compaction_cancelled_before_install());
    }
    session.revalidate_prepared_compaction_install(prepared)
}

async fn discard_staged_with_error(
    staged: StagedSessionBundle,
    error: RuntimeError,
) -> RuntimeError {
    match staged.discard().await {
        Ok(()) => error,
        Err(discard_error) => discard_error.into(),
    }
}

fn compaction_cancelled_before_install() -> RuntimeError {
    RuntimeError::CompactionModelStream {
        message: "compaction cancelled before checkpoint install".to_owned(),
    }
}
