//! One side call that names a session from its first user message.
//!
//! The call is not part of the coding request. It uses the summary role when
//! that role is configured, starts after the first user message is recorded,
//! and does not run again if it fails or times out. Dropping the owning
//! [`TopicTask`] cancels the token and aborts the background task.

use std::{sync::Arc, time::Duration};

use merry_llm::{
    GenerationConfig, ModelContent, ModelInputItem, ModelMessage, ModelMessageRole, ModelRequest,
    ModelStreamContext,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    RuntimeModelRole,
    model_completion::{ModelCompletionError, complete_single_text},
    model_config::ModelProviderConfig,
};

use super::state::RuntimeInner;

const TOPIC_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const TOPIC_WAIT_ON_SAVE: Duration = Duration::from_secs(2);
const MAX_OUTPUT_TOKENS: u64 = 128;
const MAX_PROMPT_CHARS: usize = 4_000;
const MAX_TOPIC_CHARS: usize = 60;
const TOPIC_INSTRUCTIONS: &str = "\
Name this session in one short noun phrase of at most eight words. \
Use the user's language. Reply with the phrase only.";

/// Owned handle to an in-flight first-turn topic generation call.
///
/// Dropping this handle cancels the provider stream token and aborts the
/// background task so no orphaned task can outlive the session.
#[derive(Debug)]
pub(crate) struct TopicTask {
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl TopicTask {
    fn new(cancel: CancellationToken, handle: tokio::task::JoinHandle<()>) -> Self {
        Self { cancel, handle }
    }

    async fn wait_bounded(mut self, timeout: Duration) {
        if tokio::time::timeout(timeout, &mut self.handle)
            .await
            .is_err()
        {
            // `Drop` cancels the token and aborts the task.
            tracing::debug!("session topic wait elapsed; keeping fallback label");
        }
    }
}

impl Drop for TopicTask {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.handle.abort();
    }
}

#[derive(Debug, Error)]
enum TopicError {
    #[error("could not build session topic request: {0}")]
    BuildRequest(#[from] merry_llm::ModelError),
    #[error("session topic call was cancelled")]
    Cancelled,
    #[error("session topic call timed out")]
    TimedOut,
    #[error("session topic completion failed: {0}")]
    Completion(String),
}

pub(super) async fn schedule(inner: &Arc<RuntimeInner>) {
    let Some(config) = inner.models.configs.get(RuntimeModelRole::SummaryMemory) else {
        return;
    };
    let prompt = {
        let session = inner.session.lock().await;
        if session.topic().is_some() {
            return;
        }
        let Some(text) = session.first_user_message_text() else {
            return;
        };
        bounded_chars(text.trim(), MAX_PROMPT_CHARS)
    };
    if prompt.is_empty() {
        return;
    }
    let weak_inner = Arc::downgrade(inner);
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let handle = tokio::spawn(async move {
        generate(weak_inner, config, prompt, task_cancel).await;
    });
    inner
        .session
        .lock()
        .await
        .set_topic_task(TopicTask::new(cancel, handle));
}

pub(super) async fn topic(inner: &RuntimeInner) -> Option<String> {
    let task = inner.session.lock().await.take_topic_task();
    if let Some(task) = task {
        task.wait_bounded(TOPIC_WAIT_ON_SAVE).await;
    }
    inner.session.lock().await.topic().map(str::to_owned)
}

async fn generate(
    weak_inner: std::sync::Weak<RuntimeInner>,
    config: ModelProviderConfig,
    prompt: String,
    cancel: CancellationToken,
) {
    let topic = match request_topic(&config, &prompt, &cancel).await {
        Ok(topic) => topic,
        Err(TopicError::Cancelled) => return,
        Err(error) => {
            tracing::warn!(error = %error, "session topic was not generated");
            return;
        }
    };
    let Some(topic) = normalize_topic(&topic) else {
        tracing::warn!("session topic response was empty");
        return;
    };
    let Some(inner) = weak_inner.upgrade() else {
        return;
    };
    inner.session.lock().await.set_generated_topic(topic);
    if let Some(cursor) = super::journal_persistence::write_resume_safe_savepoint(&inner).await {
        inner.session.lock().await.adopt_journal_cursor(cursor);
    }
}

async fn request_topic(
    config: &ModelProviderConfig,
    prompt: &str,
    cancel: &CancellationToken,
) -> Result<String, TopicError> {
    let system = ModelInputItem::Message(ModelMessage::new(
        ModelMessageRole::System,
        ModelContent::text(TOPIC_INSTRUCTIONS)?,
    )?);
    let user = ModelInputItem::Message(ModelMessage::new(
        ModelMessageRole::User,
        ModelContent::text(prompt)?,
    )?);
    let request = ModelRequest::new_with_input_and_stable_prefix(
        config.model().clone(),
        vec![system, user],
        Vec::new(),
        GenerationConfig::new(Some(MAX_OUTPUT_TOKENS), false)?,
        1,
    )?;
    let provider = config.provider();
    let completion = complete_single_text(
        provider.as_ref(),
        request,
        ModelStreamContext::new(cancel.clone()),
        cancel,
    );
    match tokio::time::timeout(TOPIC_REQUEST_TIMEOUT, completion).await {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(ModelCompletionError::Cancelled)) => Err(TopicError::Cancelled),
        Ok(Err(error)) => Err(TopicError::Completion(completion_error(&error))),
        Err(_) => {
            cancel.cancel();
            Err(TopicError::TimedOut)
        }
    }
}

fn completion_error(error: &ModelCompletionError) -> String {
    match error {
        ModelCompletionError::Cancelled => "cancelled".to_owned(),
        ModelCompletionError::Setup { message, .. }
        | ModelCompletionError::Stream { message, .. } => message.clone(),
        ModelCompletionError::ToolCallRequested => {
            "session topic model requested a tool".to_owned()
        }
        ModelCompletionError::NonStopFinish { finish_reason, .. } => {
            format!("session topic model finished with {finish_reason:?}")
        }
        ModelCompletionError::NotSingleText => {
            "session topic model did not return one text answer".to_owned()
        }
        ModelCompletionError::EndedBeforeCompletion => {
            "session topic model stream ended early".to_owned()
        }
    }
}

fn normalize_topic(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|text| text.strip_suffix('\''))
        })
        .unwrap_or(trimmed);
    let compact = unquoted.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }
    Some(bounded_chars(&compact, MAX_TOPIC_CHARS))
}

fn bounded_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    if max_chars <= 3 {
        return text.chars().take(max_chars).collect();
    }
    let mut bounded = text.chars().take(max_chars - 3).collect::<String>();
    bounded.push_str("...");
    bounded
}
