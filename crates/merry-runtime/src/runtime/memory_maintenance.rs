//! Host-side capture and one-shot dream.
//!
//! Maintenance is scheduled after a parent agent loop completes and does not
//! block that loop. A child runtime does not schedule it. Failure is logged
//! and never changes the user-visible turn result.

use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use merry_llm::{
    GenerationConfig, ModelContent, ModelInputItem, ModelMessage, ModelMessageRole, ModelRequest,
    ModelResponseFormat, ModelStreamContext, ModelStructuredOutputFormat,
};
use schemars::Schema;
use tokio_util::sync::CancellationToken;

use crate::{
    RuntimeModelRole,
    durable_memory::{
        DurableMemory, DurableMemoryError,
        capture::{self, CaptureOutcome, CaptureWrite},
        condense::{self, count_external_turns},
        dream::{self, CLAIM_LIMIT, dream_should_run},
    },
    model_completion::{ModelCompletionError, complete_single_text},
    model_config::ModelProviderConfig,
    session::TranscriptItemSnapshot,
};

use super::state::RuntimeInner;

const CAPTURE_ATTEMPTS: u32 = 3;
const DREAM_PASSES: u32 = 3;
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(120);
const DREAM_TIMEOUT: Duration = Duration::from_secs(240);
const MAX_OUTPUT_TOKENS: u64 = 16_384;

pub(super) fn schedule(inner: &Arc<RuntimeInner>) {
    if !inner.memory_maintenance {
        return;
    }
    let Some(memory) = inner.durable_memory.clone() else {
        return;
    };
    let inner = Arc::clone(inner);
    tokio::spawn(async move {
        let _queued = memory.maintenance().lock().await;
        maintain(&inner, &memory).await;
    });
}

async fn maintain(inner: &Arc<RuntimeInner>, memory: &Arc<DurableMemory>) {
    let Some(config) = inner
        .model_config_with_primary_fallback(RuntimeModelRole::SummaryMemory)
        .await
    else {
        tracing::warn!("durable memory maintenance skipped because no model is configured");
        return;
    };
    let transcript = {
        let session = inner.session.lock().await;
        session.full_transcript_snapshot()
    };
    let session_id = inner.session_id.as_str().to_owned();
    let turns = count_external_turns(&transcript);
    let cursor = match blocking(Arc::clone(memory), {
        let session_id = session_id.clone();
        move |memory| memory.captured_turns(&session_id)
    })
    .await
    {
        Ok(cursor) => cursor,
        Err(error) => {
            tracing::warn!(error = %error, "durable memory cursor was not read");
            return;
        }
    };
    let scan = match blocking(Arc::clone(memory), |memory| memory.scan_inbox()).await {
        Ok(scan) => scan,
        Err(error) => {
            tracing::warn!(error = %error, "durable memory inbox was not scanned");
            return;
        }
    };
    let needs_capture = turns > cursor;
    let needs_dream = dream_should_run(&scan, SystemTime::now());
    if !needs_capture && !needs_dream {
        return;
    }
    let owner = format!("{}:{session_id}", std::process::id());
    let acquired = match blocking(Arc::clone(memory), {
        let owner = owner.clone();
        move |memory| memory.try_acquire_lease(&owner)
    })
    .await
    {
        Ok(acquired) => acquired,
        Err(error) => {
            tracing::warn!(error = %error, "durable memory lease was not acquired");
            return;
        }
    };
    if !acquired {
        tracing::info!("durable memory maintenance coalesced behind another lease");
        return;
    }
    let _lease = LeaseGuard {
        memory: Arc::clone(memory),
        owner: owner.clone(),
    };
    if needs_capture {
        capture_range(
            memory,
            &config,
            &owner,
            &session_id,
            &transcript,
            cursor.saturating_add(1),
            turns,
        )
        .await;
    }
    if needs_dream || capture_added_notes(memory).await {
        consolidate(memory, &config, &owner).await;
    }
}

async fn capture_added_notes(memory: &Arc<DurableMemory>) -> bool {
    match blocking(Arc::clone(memory), |memory| memory.scan_inbox()).await {
        Ok(scan) => dream_should_run(&scan, SystemTime::now()),
        Err(error) => {
            tracing::warn!(error = %error, "durable memory inbox was not rescanned");
            false
        }
    }
}

async fn capture_range(
    memory: &Arc<DurableMemory>,
    config: &ModelProviderConfig,
    owner: &str,
    session_id: &str,
    transcript: &[TranscriptItemSnapshot],
    from_turn: u32,
    through_turn: u32,
) {
    if blocking(Arc::clone(memory), {
        let session_id = session_id.to_owned();
        move |memory| Ok(memory.observation_exists(&session_id, from_turn, through_turn))
    })
    .await
    .unwrap_or(false)
    {
        advance_cursor(memory, session_id, through_turn).await;
        return;
    }
    for attempt in 1..=CAPTURE_ATTEMPTS {
        if !refresh_lease(memory, owner).await {
            return;
        }
        let condensed = match condense::condense_turn_range(transcript, from_turn, through_turn) {
            Ok(condensed) => condensed,
            Err(error) => {
                tracing::warn!(error = %error, "durable memory transcript was not condensed");
                advance_cursor(memory, session_id, through_turn).await;
                return;
            }
        };
        let (system, user) = capture::extraction_request_text(
            condensed.note.as_deref(),
            session_id,
            from_turn,
            through_turn,
            &condensed.json,
        );
        let text = match complete_memory(
            config,
            "memory_capture",
            capture::extraction_schema(),
            &system,
            &user,
            CAPTURE_TIMEOUT,
        )
        .await
        {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(attempt, error = %error, "durable memory capture attempt failed");
                continue;
            }
        };
        match capture::parse_model_outcome(&text) {
            Ok(CaptureOutcome::Noop) => {
                advance_cursor(memory, session_id, through_turn).await;
                return;
            }
            Ok(CaptureOutcome::Observations(observations)) => {
                let model = config.model().as_str().to_owned();
                let session_owned = session_id.to_owned();
                let job_id = format!("capture-{session_owned}-{from_turn}-{through_turn}");
                let committed = blocking(Arc::clone(memory), move |memory| {
                    let write = CaptureWrite {
                        session_id: &session_owned,
                        from_turn,
                        through_turn,
                        extraction_model: &model,
                        created_at: i64::try_from(crate::durable_memory::state::unix_now())
                            .unwrap_or(i64::MAX),
                        job_id: &job_id,
                        observations: &observations,
                    };
                    memory.commit_capture(&write)
                })
                .await;
                match committed {
                    Ok(()) => {
                        advance_cursor(memory, session_id, through_turn).await;
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(
                            attempt,
                            error = %error,
                            "durable memory observations were not written"
                        );
                    }
                }
            }
            Err(error) => {
                tracing::warn!(attempt, error = %error, "durable memory extraction was rejected");
            }
        }
    }
    tracing::warn!(
        from_turn,
        through_turn,
        "durable memory dropped an uncaptured turn after the final attempt"
    );
    advance_cursor(memory, session_id, through_turn).await;
}

async fn consolidate(memory: &Arc<DurableMemory>, config: &ModelProviderConfig, owner: &str) {
    for _pass in 0..DREAM_PASSES {
        if !refresh_lease(memory, owner).await {
            return;
        }
        let scan = match blocking(Arc::clone(memory), |memory| memory.scan_inbox()).await {
            Ok(scan) => scan,
            Err(error) => {
                tracing::warn!(error = %error, "durable memory dream scan failed");
                return;
            }
        };
        if !dream_should_run(&scan, SystemTime::now()) {
            return;
        }
        let claimed = scan
            .eligible
            .into_iter()
            .take(CLAIM_LIMIT)
            .collect::<Vec<_>>();
        if claimed.is_empty() {
            return;
        }
        let input = match blocking(Arc::clone(memory), {
            let claimed = claimed.clone();
            move |memory| memory.dream_input(&claimed)
        })
        .await
        {
            Ok(input) => input,
            Err(error) => {
                tracing::warn!(error = %error, "durable memory dream input was not built");
                return;
            }
        };
        let text = match complete_memory(
            config,
            "memory_dream",
            dream::dream_plan_schema(),
            dream::DREAM_SYSTEM_PROMPT,
            &input,
            DREAM_TIMEOUT,
        )
        .await
        {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(error = %error, "durable memory dream model call failed");
                return;
            }
        };
        match blocking(Arc::clone(memory), {
            let claimed = claimed.clone();
            let text = text.clone();
            move |memory| memory.apply_dream(&claimed, &text)
        })
        .await
        {
            Ok(operations) => {
                tracing::info!(
                    operations,
                    notes = claimed.len(),
                    "durable memory dream applied"
                );
            }
            Err(error) => {
                tracing::warn!(error = %error, "durable memory dream plan was rejected");
                return;
            }
        }
    }
}

async fn advance_cursor(memory: &Arc<DurableMemory>, session_id: &str, through_turn: u32) {
    let session_id = session_id.to_owned();
    if let Err(error) = blocking(Arc::clone(memory), move |memory| {
        memory.set_captured_turns(&session_id, through_turn)
    })
    .await
    {
        tracing::warn!(error = %error, "durable memory cursor was not advanced");
    }
}

async fn refresh_lease(memory: &Arc<DurableMemory>, owner: &str) -> bool {
    let owner = owner.to_owned();
    match blocking(Arc::clone(memory), move |memory| {
        memory.try_acquire_lease(&owner)
    })
    .await
    {
        Ok(true) => true,
        Ok(false) => {
            tracing::info!("durable memory lease was lost");
            false
        }
        Err(error) => {
            tracing::warn!(error = %error, "durable memory lease was not refreshed");
            false
        }
    }
}

async fn complete_memory(
    config: &ModelProviderConfig,
    schema_name: &str,
    schema: serde_json::Value,
    system: &str,
    user: &str,
    timeout: Duration,
) -> Result<String, String> {
    let schema =
        Schema::try_from(schema).map_err(|error| format!("memory schema was invalid: {error}"))?;
    let format =
        ModelStructuredOutputFormat::new(schema_name, schema).map_err(|error| error.to_string())?;
    let system_message = ModelInputItem::Message(
        ModelMessage::new(
            ModelMessageRole::System,
            ModelContent::text(system).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?,
    );
    let user_message = ModelInputItem::Message(
        ModelMessage::new(
            ModelMessageRole::User,
            ModelContent::text(user).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?,
    );
    let request = ModelRequest::new_with_input_and_stable_prefix_and_response_format(
        config.model().clone(),
        vec![system_message, user_message],
        Vec::new(),
        GenerationConfig::new(Some(MAX_OUTPUT_TOKENS), false).map_err(|error| error.to_string())?,
        1,
        Some(ModelResponseFormat::StructuredOutput(format)),
    )
    .map_err(|error| error.to_string())?;
    let token = CancellationToken::new();
    let provider = config.provider();
    let completion = complete_single_text(
        provider.as_ref(),
        request,
        ModelStreamContext::new(token.clone()),
        &token,
    );
    match tokio::time::timeout(timeout, completion).await {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(error)) => Err(completion_error(&error)),
        Err(_) => Err("memory model call timed out".to_owned()),
    }
}

fn completion_error(error: &ModelCompletionError) -> String {
    match error {
        ModelCompletionError::Cancelled => "memory model call was cancelled".to_owned(),
        ModelCompletionError::Setup { message, .. }
        | ModelCompletionError::Stream { message, .. } => message.clone(),
        ModelCompletionError::ToolCallRequested => "memory model requested a tool".to_owned(),
        ModelCompletionError::NonStopFinish { finish_reason, .. } => {
            format!("memory model finished with {finish_reason:?}")
        }
        ModelCompletionError::NotSingleText => {
            "memory model did not return one text answer".to_owned()
        }
        ModelCompletionError::EndedBeforeCompletion => "memory model stream ended early".to_owned(),
    }
}

async fn blocking<T: Send + 'static>(
    memory: Arc<DurableMemory>,
    work: impl FnOnce(&DurableMemory) -> Result<T, DurableMemoryError> + Send + 'static,
) -> Result<T, DurableMemoryError> {
    match tokio::task::spawn_blocking(move || work(&memory)).await {
        Ok(result) => result,
        Err(_) => Err(DurableMemoryError::Denied {
            message: "memory task stopped".to_owned(),
        }),
    }
}

struct LeaseGuard {
    memory: Arc<DurableMemory>,
    owner: String,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if let Err(error) = self.memory.release_lease(&self.owner) {
            tracing::warn!(error = %error, "durable memory lease was not released");
        }
    }
}
