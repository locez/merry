//! Parent-session event sink and persisted subagent record contracts.
//!
//! Bridges `SubagentManager` lifecycle transitions and usage rollups back into
//! the parent session's durable journal and live broadcast stream without
//! creating a circular dependency on `Runtime`.

use super::manager::{
    ManagedSubagent, SUBAGENT_EVENT_CHANNEL_CAPACITY, SubagentInspectSession, SubagentManager,
    SubagentManagerState,
};
use super::protocol::{
    SubagentResultView, SubagentStatusLabel, SubagentStatusView, SubagentWorkspaceMode,
    SubagentWorktreeView,
};
use super::scope::{ChildWorkspaceFinalization, ChildWorkspaceLifecycle};
use super::spec::SubagentTaskSpec;
use crate::{
    FileSessionStore, LoadedSession, Runtime, SessionTranscriptItem, TaskAnchor,
    session::SessionState, trajectory::RuntimeObservability,
};
use merry_core::{
    ErrorInfo, ModelUsage, RuntimeJournalEvent, RuntimeJournalPayload, SessionId, SubagentId,
    SubagentStatus, SubagentTaskId,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;

/// Compact, durable record of one subagent persisted inside the parent's `journal.jsonl`.
///
/// Restored on parent session resume so TUI/CLI/SDK surfaces immediately display
/// historical subagent cards and can lazily load or wake each child from
/// `<sessions_dir>/<parent_id>/subagents/<agent_id>/journal.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedSubagentRecord {
    pub(crate) agent_id: SubagentId,
    pub(crate) task_id: SubagentTaskId,
    pub(crate) task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) role: Option<String>,
    pub(crate) status: SubagentStatusLabel,
    pub(crate) summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) conclusion: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) output_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) changed_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) diagnostic: Option<ErrorInfo>,
    #[serde(default)]
    pub(crate) turns_used: u32,
    #[serde(default)]
    pub(crate) tool_calls: u32,
    #[serde(default)]
    pub(crate) elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) usage: Option<ModelUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) worktree: Option<SubagentWorktreeView>,
}

impl PersistedSubagentRecord {
    pub(crate) fn from_status_view(task: String, view: &SubagentStatusView) -> Self {
        Self {
            agent_id: view.agent_id.clone(),
            task_id: view.task_id.clone(),
            task,
            display_name: view.display_name.clone(),
            role: view.role.clone(),
            status: view.status.clone(),
            summary: view.summary.clone(),
            conclusion: view.result.as_ref().map(|res| res.conclusion.clone()),
            output_paths: view.output_paths.clone(),
            changed_paths: view.changed_paths.clone(),
            diagnostic: view.diagnostics.clone(),
            turns_used: view.turns_used,
            tool_calls: view.tool_calls,
            elapsed_ms: view.elapsed_ms,
            usage: view.usage,
            worktree: view.worktree.clone(),
        }
    }
}

/// Converts internal runtime subagent status label into public [`SubagentStatus`].
pub(crate) fn to_public_subagent_status(status: &SubagentStatusLabel) -> SubagentStatus {
    match status {
        SubagentStatusLabel::Queued => SubagentStatus::Queued,
        SubagentStatusLabel::Running => SubagentStatus::Running,
        SubagentStatusLabel::Completed => SubagentStatus::Completed,
        SubagentStatusLabel::Failed => SubagentStatus::Failed,
        SubagentStatusLabel::Cancelled => SubagentStatus::Cancelled,
        SubagentStatusLabel::Blocked => SubagentStatus::Blocked,
    }
}

/// Lightweight parent-owned event and persistence sink injected into [`super::SubagentManager`].
#[derive(Clone)]
pub(crate) struct SubagentEventSink {
    session: Arc<Mutex<SessionState>>,
    session_store: Option<FileSessionStore>,
    trajectory: Arc<RuntimeObservability>,
    event_tx: broadcast::Sender<RuntimeJournalEvent>,
}

impl SubagentEventSink {
    pub(crate) fn new(
        session: Arc<Mutex<SessionState>>,
        session_store: Option<FileSessionStore>,
        trajectory: Arc<RuntimeObservability>,
        capacity: usize,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(capacity.max(64));
        Self {
            session,
            session_store,
            trajectory,
            event_tx,
        }
    }

    /// Subscribes to live journal events emitted asynchronously by managed subagents.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RuntimeJournalEvent> {
        self.event_tx.subscribe()
    }

    /// Appends one journal payload onto the parent session and broadcasts it live.
    pub(crate) async fn emit_journal_payload(
        &self,
        payload: RuntimeJournalPayload,
    ) -> RuntimeJournalEvent {
        let event = {
            let mut session = self.session.lock().await;
            session.record_event(payload)
        };
        self.trajectory.observe_journal_event(&event);
        let _ = self.event_tx.send(event.clone());
        event
    }

    /// Folds incremental subagent token usage into the parent session's usage ledger
    /// and broadcasts the updated `SessionUsageUpdated` event.
    pub(crate) async fn record_subagent_usage(
        &self,
        delta: ModelUsage,
    ) -> Option<RuntimeJournalEvent> {
        if delta.input_tokens == 0 && delta.output_tokens == 0 && delta.total_tokens == 0 {
            return None;
        }
        let recorded = {
            let mut session = self.session.lock().await;
            session.record_subagent_usage(delta)
        };
        match recorded {
            Ok(event) => {
                self.trajectory.observe_journal_event(&event);
                let _ = self.event_tx.send(event.clone());
                Some(event)
            }
            Err(diagnostic) => {
                tracing::warn!(
                    code = diagnostic.code(),
                    message = diagnostic.message(),
                    "failed to fold subagent usage into parent session"
                );
                None
            }
        }
    }

    /// Records or updates one persisted subagent snapshot in parent `SessionState`
    /// and flushes a resume-safe savepoint when the parent has a `FileSessionStore`.
    pub(crate) async fn upsert_persisted_subagent(&self, record: PersistedSubagentRecord) {
        let prepared = {
            let mut session = self.session.lock().await;
            session.upsert_persisted_subagent(record);
            self.session_store
                .as_ref()
                .map(|_| session.persistable_bundle_if_resume_safe())
        };
        let (Some(store), Some(Ok(Some(bundle)))) = (self.session_store.as_ref(), prepared) else {
            return;
        };
        let cursor = bundle.cursor.clone();
        if let Err(error) = store.write_bundle(bundle).await {
            tracing::warn!(
                error = %error,
                "failed to flush parent session savepoint after subagent state update"
            );
            return;
        }
        self.session.lock().await.adopt_journal_cursor(cursor);
    }
}

/// Computes the positive token usage delta between a previous cumulative child
/// `ModelUsage` snapshot and a newer cumulative snapshot.
pub(crate) fn usage_delta(previous: Option<ModelUsage>, current: ModelUsage) -> Option<ModelUsage> {
    let Some(prev) = previous else {
        return (current.input_tokens > 0 || current.output_tokens > 0 || current.total_tokens > 0)
            .then_some(current);
    };
    let input_tokens = current.input_tokens.saturating_sub(prev.input_tokens);
    let output_tokens = current.output_tokens.saturating_sub(prev.output_tokens);
    let total_tokens = current.total_tokens.saturating_sub(prev.total_tokens);
    let cached_input_tokens = match (current.cached_input_tokens, prev.cached_input_tokens) {
        (Some(cur), Some(old)) => Some(cur.saturating_sub(old)),
        (Some(cur), None) => Some(cur),
        (None, _) => None,
    };
    let reasoning_output_tokens = match (
        current.reasoning_output_tokens,
        prev.reasoning_output_tokens,
    ) {
        (Some(cur), Some(old)) => Some(cur.saturating_sub(old)),
        (Some(cur), None) => Some(cur),
        (None, _) => None,
    };
    if input_tokens == 0 && output_tokens == 0 && total_tokens == 0 {
        return None;
    }
    Some(ModelUsage::with_details(
        input_tokens,
        cached_input_tokens,
        output_tokens,
        reasoning_output_tokens,
        total_tokens,
    ))
}

/// Persists the final subagent snapshot into the parent session and emits the
/// corresponding terminal `RuntimeJournalPayload::Subagent*` event.
pub(crate) async fn emit_terminal_event_and_persist(
    sink: Option<&SubagentEventSink>,
    task: String,
    view: &SubagentStatusView,
) {
    let Some(sink) = sink else {
        return;
    };
    let payload = match view.status {
        SubagentStatusLabel::Completed => RuntimeJournalPayload::SubagentCompleted {
            agent_id: view.agent_id.clone(),
            task_id: view.task_id.clone(),
            summary: view.summary.clone(),
            output_paths: view.output_paths.clone(),
            changed_paths: view.changed_paths.clone(),
            usage: view.usage,
        },
        SubagentStatusLabel::Failed => RuntimeJournalPayload::SubagentFailed {
            agent_id: view.agent_id.clone(),
            task_id: view.task_id.clone(),
            diagnostic: view.diagnostics.clone().unwrap_or_else(|| {
                ErrorInfo::new("subagent_failed", &view.summary)
                    .expect("static diagnostic code is valid")
            }),
        },
        SubagentStatusLabel::Cancelled => RuntimeJournalPayload::SubagentCancelled {
            agent_id: view.agent_id.clone(),
            task_id: view.task_id.clone(),
            diagnostic: view.diagnostics.clone().unwrap_or_else(|| {
                ErrorInfo::new("subagent_cancelled", &view.summary)
                    .expect("static diagnostic code is valid")
            }),
        },
        SubagentStatusLabel::Blocked => RuntimeJournalPayload::SubagentStatusChanged {
            agent_id: view.agent_id.clone(),
            task_id: view.task_id.clone(),
            status: SubagentStatus::Blocked,
        },
        SubagentStatusLabel::Queued | SubagentStatusLabel::Running => {
            RuntimeJournalPayload::SubagentStatusChanged {
                agent_id: view.agent_id.clone(),
                task_id: view.task_id.clone(),
                status: to_public_subagent_status(&view.status),
            }
        }
    };
    sink.emit_journal_payload(payload).await;
    sink.upsert_persisted_subagent(PersistedSubagentRecord::from_status_view(task, view))
        .await;
}

/// Timeout for flushing a child's durable session journal during teardown.
const CHILD_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);
/// Timeout for folding subagent token usage into the parent session during teardown.
const CHILD_USAGE_ROLLUP_TIMEOUT: Duration = Duration::from_secs(2);
/// Timeout for collecting isolated worktree diffs and running fail-toward-keep cleanup.
const CHILD_WORKSPACE_FINALIZE_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs an isolated child workspace finalization hook on a blocking worker thread
/// with a bounded timeout, failing toward keeping the workspace on timeout/panic.
pub(super) async fn finalize_child_workspace(
    hook: Option<Arc<dyn ChildWorkspaceLifecycle>>,
    allow_cleanup: bool,
) -> Option<ChildWorkspaceFinalization> {
    let hook = hook?;
    let task = tokio::task::spawn_blocking(move || hook.finalize(allow_cleanup));
    match tokio::time::timeout(CHILD_WORKSPACE_FINALIZE_TIMEOUT, task).await {
        Ok(Ok(finalization)) => Some(finalization),
        Ok(Err(error)) => {
            tracing::warn!(
                error = %error,
                "child workspace finalization task panicked; failing toward keeping workspace"
            );
            None
        }
        Err(_) => {
            tracing::warn!(
                timeout_secs = CHILD_WORKSPACE_FINALIZE_TIMEOUT.as_secs(),
                "child workspace finalization timed out; failing toward keeping workspace"
            );
            None
        }
    }
}

/// Merges a finalized isolated workspace result into one [`ManagedSubagent`] in place.
pub(super) fn apply_workspace_finalization(
    agent: &mut ManagedSubagent,
    finalization: Option<ChildWorkspaceFinalization>,
) {
    let Some(finalization) = finalization else {
        return;
    };
    if !finalization.changed_paths.is_empty() {
        let mut merged = agent
            .changed_paths
            .iter()
            .cloned()
            .collect::<BTreeSet<String>>();
        merged.extend(finalization.changed_paths);
        agent.changed_paths = merged.into_iter().collect();
    }
    let removed = finalization
        .worktree
        .as_ref()
        .is_some_and(|worktree| !worktree.retained);
    if finalization.worktree.is_some() {
        agent.worktree = finalization.worktree;
    }
    if removed {
        // The isolated worktree directory was deleted after zero net changes;
        // clear the cached runtime so a future wake creates a fresh worktree
        // instead of executing tools inside a deleted directory.
        agent.runtime = None;
        agent.workspace_lifecycle = None;
    }
}

/// Finalizes one agent's isolated workspace (if any) without holding the manager
/// state lock across blocking git operations, then merges discovered paths and
/// updated worktree metadata back into manager state.
pub(super) async fn finalize_agent_workspace_into_state(
    state: &Mutex<SubagentManagerState>,
    agent_id: &SubagentId,
    allow_cleanup: bool,
) {
    let hook = {
        let guard = state.lock().await;
        guard
            .agents
            .get(agent_id)
            .and_then(|agent| agent.workspace_lifecycle.clone())
    };
    let finalization = finalize_child_workspace(hook, allow_cleanup).await;
    let mut guard = state.lock().await;
    if let Some(agent) = guard.agents.get_mut(agent_id) {
        apply_workspace_finalization(agent, finalization);
    }
}

/// Executes the strict 4-step child teardown sequence:
/// 1. Flush child durable session store (bounded timeout)
/// 2. Reconcile any remaining unreported token usage into parent (bounded timeout + fallback)
/// 3. Clean up any surviving background processes owned by the child
/// 4. Persist final parent subagent snapshot and emit parent terminal journal event
pub(crate) async fn teardown_child_runtime(
    sink: Option<&SubagentEventSink>,
    child_runtime: &Runtime,
    reported_usage: Option<ModelUsage>,
    final_usage: Option<ModelUsage>,
    task: String,
    view: &SubagentStatusView,
) {
    // Step 1: Flush child journal so immediate wake/inspect sees all turns on disk.
    match tokio::time::timeout(
        CHILD_FLUSH_TIMEOUT,
        child_runtime.flush_attached_session_store(),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(
                subagent_id = %view.agent_id,
                error = %error,
                "failed to flush child session store during teardown"
            );
        }
        Err(_) => {
            tracing::warn!(
                subagent_id = %view.agent_id,
                timeout_secs = CHILD_FLUSH_TIMEOUT.as_secs(),
                "timed out flushing child session store during teardown"
            );
        }
    }

    // Step 2: Reconcile any un-reported usage delta with bounded timeout + fallback.
    if let (Some(sink), Some(final_total)) = (sink, final_usage)
        && let Some(delta) = usage_delta(reported_usage, final_total)
        && tokio::time::timeout(
            CHILD_USAGE_ROLLUP_TIMEOUT,
            sink.record_subagent_usage(delta),
        )
        .await
        .is_err()
    {
        tracing::warn!(
            subagent_id = %view.agent_id,
            timeout_secs = CHILD_USAGE_ROLLUP_TIMEOUT.as_secs(),
            "timed out folding subagent token usage into parent session; continuing teardown"
        );
    }

    // Step 3: Stop any surviving background processes spawned by the child.
    child_runtime.shutdown_background_processes();

    // Step 4: Persist parent subagent record and emit terminal parent journal event.
    emit_terminal_event_and_persist(sink, task, view).await;
}

/// Folds one incremental child usage delta into the parent session with bounded timeout.
pub(crate) async fn fold_incremental_child_usage(
    sink: Option<&SubagentEventSink>,
    reported_usage: &mut Option<ModelUsage>,
    current_total: ModelUsage,
) {
    let Some(delta) = usage_delta(*reported_usage, current_total) else {
        return;
    };
    *reported_usage = Some(current_total);
    if let Some(sink) = sink
        && tokio::time::timeout(
            CHILD_USAGE_ROLLUP_TIMEOUT,
            sink.record_subagent_usage(delta),
        )
        .await
        .is_err()
    {
        tracing::warn!(
            timeout_secs = CHILD_USAGE_ROLLUP_TIMEOUT.as_secs(),
            "timed out recording incremental subagent token usage"
        );
    }
}

/// Returns the deterministic child [`SessionId`] used to store the child's
/// journal at `<sessions_dir>/<parent_id>/subagents/<agent_id>/journal.jsonl`.
pub(crate) fn child_session_id_for_agent(agent_id: &SubagentId) -> SessionId {
    SessionId::new(agent_id.as_str()).expect("valid SubagentId is always a valid SessionId")
}

impl SubagentManager {
    /// Attaches the parent-session event/persistence sink so subagent lifecycle
    /// transitions and usage rollups enter the parent's durable journal and live streams.
    pub(crate) fn attach_event_sink(&self, sink: SubagentEventSink) {
        *self
            .event_sink
            .lock()
            .expect("subagent event sink mutex is not poisoned") = Some(sink);
    }

    pub(super) fn attached_event_sink(&self) -> Option<SubagentEventSink> {
        self.event_sink
            .lock()
            .expect("subagent event sink mutex is not poisoned")
            .clone()
    }

    /// Attaches the child-scoped session store (`<sessions_dir>/<parent_id>/subagents`).
    pub(crate) fn attach_child_session_store(&self, store: FileSessionStore) {
        *self
            .child_session_store
            .lock()
            .expect("subagent child session store mutex is not poisoned") = Some(store);
    }

    pub(super) fn attached_child_session_store(&self) -> Option<FileSessionStore> {
        self.child_session_store
            .lock()
            .expect("subagent child session store mutex is not poisoned")
            .clone()
    }

    /// Hydrates historical subagent entries persisted in the parent's `journal.jsonl`
    /// when resuming a session, advancing `next_id` past all restored numeric suffixes.
    pub(crate) fn hydrate_persisted(&self, records: Vec<PersistedSubagentRecord>) {
        if records.is_empty() {
            return;
        }
        let mut max_numeric_id: u64 = 0;
        let Ok(mut state) = self.state.try_lock() else {
            return;
        };
        for record in records {
            if let Some(num) = record
                .agent_id
                .as_str()
                .strip_prefix("agent-")
                .or_else(|| record.agent_id.as_str().strip_prefix("subagent-"))
                .and_then(|suffix| suffix.parse::<u64>().ok())
            {
                max_numeric_id = max_numeric_id.max(num);
            }
            if state.agents.contains_key(&record.agent_id) {
                continue;
            }
            let (status, summary, diagnostics) = if record.status.is_terminal() {
                (record.status, record.summary, record.diagnostic)
            } else {
                (
                    SubagentStatusLabel::Cancelled,
                    "interrupted by previous session exit".to_owned(),
                    Some(
                        ErrorInfo::new(
                            "subagent_interrupted",
                            "subagent was still active when the previous session exited; send a message to resume",
                        )
                        .expect("static diagnostic code is valid"),
                    ),
                )
            };
            let task_text = if record.task.trim().is_empty() {
                summary.clone()
            } else {
                record.task
            };
            let mut task_spec =
                SubagentTaskSpec::new(task_text.clone(), self.max_model_turns().max(1))
                    .unwrap_or_else(|_| {
                        SubagentTaskSpec::new("resumed subagent task", 1)
                            .expect("static task is valid")
                    });
            if let Some(name) = record.display_name {
                task_spec = task_spec.with_display_name(Some(name));
            }
            if let Some(role) = record.role
                && let Ok(updated) = task_spec.clone().with_role(Some(role))
            {
                task_spec = updated;
            }
            if record.worktree.is_some() {
                task_spec = task_spec.with_workspace_mode(SubagentWorkspaceMode::Worktree);
            }
            let task_anchor = TaskAnchor::new(&task_text)
                .unwrap_or_else(|_| TaskAnchor::new("resumed subagent task").expect("valid"));
            let result = record
                .conclusion
                .and_then(SubagentResultView::from_conclusion);
            let (event_tx, _) = broadcast::channel(SUBAGENT_EVENT_CHANNEL_CAPACITY);
            state.agents.insert(
                record.agent_id.clone(),
                ManagedSubagent {
                    batch_id: 0,
                    agent_id: record.agent_id,
                    task_id: record.task_id,
                    task: task_spec,
                    task_anchor,
                    status,
                    summary,
                    result,
                    output_paths: record.output_paths,
                    changed_paths: record.changed_paths,
                    diagnostics,
                    cancellation_token: CancellationToken::new(),
                    plan_link: None,
                    completion_notification_acknowledged: true,
                    runtime: None,
                    generation_config: None,
                    event_tx,
                    pending_follow_ups: VecDeque::new(),
                    resume_turn_override: None,
                    started_at: None,
                    turns_used: record.turns_used,
                    tool_calls: record.tool_calls,
                    elapsed_ms: record.elapsed_ms,
                    usage: record.usage,
                    raw_output_bytes: 0,
                    workspace_lifecycle: None,
                    worktree: record.worktree,
                },
            );
        }
        if max_numeric_id > 0 {
            self.next_id.fetch_max(
                max_numeric_id.saturating_add(1),
                std::sync::atomic::Ordering::AcqRel,
            );
        }
        self.has_agents.store(
            !state.agents.is_empty(),
            std::sync::atomic::Ordering::Release,
        );
    }

    /// Returns a lazy transcript snapshot and live event subscription for one subagent.
    pub async fn inspect(&self, agent_id: &SubagentId) -> Option<SubagentInspectSession> {
        let (status, task, runtime, events) = {
            let state = self.state.lock().await;
            let agent = state.agents.get(agent_id)?;
            (
                agent.status_view(),
                agent.task.task().to_owned(),
                agent.runtime.clone(),
                agent.event_tx.subscribe(),
            )
        };
        let transcript = match runtime {
            Some(runtime) => runtime.session_transcript().await.unwrap_or_default(),
            None => self.lazy_load_child_transcript(agent_id).await,
        };
        Some(SubagentInspectSession {
            status,
            task,
            transcript,
            events,
        })
    }

    /// Lazily loads a persisted child's transcript from `<sessions_dir>/<parent_id>/subagents/<agent_id>/journal.jsonl`.
    pub(super) async fn lazy_load_child_transcript(
        &self,
        agent_id: &SubagentId,
    ) -> Vec<SessionTranscriptItem> {
        let Some(store) = self.attached_child_session_store() else {
            return Vec::new();
        };
        let session_id = child_session_id_for_agent(agent_id);
        match LoadedSession::load(&store, &session_id).await {
            Ok(loaded) => loaded.transcript_items(),
            Err(_) => Vec::new(),
        }
    }

    /// Cancels all live children owned by this manager and awaits their task
    /// handles up to a bounded shutdown timeout.
    pub async fn shutdown(&self) {
        self.session_token.cancel();
        let handles = {
            let mut state = self.state.lock().await;
            for agent in state.agents.values_mut() {
                if !agent.status.is_terminal() {
                    agent.cancellation_token.cancel();
                    agent.status = SubagentStatusLabel::Cancelled;
                    agent.summary = "child cancelled during parent shutdown".to_owned();
                    agent.diagnostics = Some(super::manager_helpers::error_info(
                        "subagent_cancelled",
                        "child cancelled during parent session shutdown",
                    ));
                }
            }
            std::mem::take(&mut state.handles)
                .into_values()
                .collect::<Vec<_>>()
        };
        self.notify.notify_waiters();
        self.completion_notify.notify_waiters();

        const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
        for mut handle in handles {
            if tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut handle)
                .await
                .is_err()
            {
                handle.abort();
            }
        }
    }
}
