use crate::{
    Runtime, StepContext, StepInput,
    runtime::tests::support::{
        common::{model_name, session_id},
        model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
    },
};
use futures_util::StreamExt;
use merry_core::{ModelOutputRate, ProviderName, RuntimeEvent, RuntimeJournalPayload};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelOutput,
    ModelOutputProgress, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, Usage,
};
use std::{num::NonZeroUsize, sync::Arc, time::Duration};
use tokio::sync::Notify;

struct CompletionNotifyingProvider {
    inner: RecordingModelProvider,
    completed: Arc<Notify>,
}

impl ModelProvider for CompletionNotifyingProvider {
    fn name(&self) -> &ProviderName {
        self.inner.name()
    }

    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            let completed = Arc::clone(&self.completed);
            let stream = self.inner.stream_model(request, context).await?;
            let stream: ModelEventStream = Box::pin(stream.inspect(move |event| {
                if matches!(event, Ok(ModelEvent::Completed { .. })) {
                    completed.notify_one();
                }
            }));
            Ok(stream)
        })
    }
}

fn progress() -> ModelEvent {
    ModelEvent::OutputProgress {
        progress: Some(
            ModelOutputProgress::new(1_600, Duration::from_secs(8)).with_reasoning_observed(true),
        ),
    }
}

fn completed(usage: Option<Usage>) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, usage),
    }
}

async fn rates(runtime: &Runtime) -> Vec<Option<ModelOutputRate>> {
    runtime
        .stream(
            StepInput::user_text("continue").unwrap(),
            StepContext::default(),
        )
        .unwrap()
        .filter_map(|event| async move {
            match event {
                RuntimeEvent::ModelOutputRateUpdated { rate, .. } => Some(rate),
                _ => None,
            }
        })
        .collect()
        .await
}

#[tokio::test]
async fn full_single_slot_buffer_does_not_block_replaceable_rate_updates() {
    let completed = Arc::new(Notify::new());
    let events = (0..10_000)
        .map(|_| Ok(progress()))
        .chain([Ok(self::completed(Some(Usage::new(100, 2_400))))])
        .collect();
    let provider = CompletionNotifyingProvider {
        inner: RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(
            events,
        )]),
        completed: Arc::clone(&completed),
    };
    let runtime = Runtime::builder(session_id("single-slot-rate-pressure"))
        .model_provider(Arc::new(provider), model_name())
        .event_buffer_size(NonZeroUsize::new(1).unwrap())
        .build()
        .unwrap();
    let mut stream = runtime
        .step(
            StepInput::user_text("continue").unwrap(),
            StepContext::default(),
        )
        .unwrap();
    assert!(matches!(
        stream.next().await.unwrap().payload,
        RuntimeJournalPayload::SessionStarted
    ));
    assert!(matches!(
        stream.next().await.unwrap().payload,
        RuntimeJournalPayload::StepStarted
    ));

    tokio::time::timeout(Duration::from_secs(2), completed.notified())
        .await
        .unwrap();

    let events: Vec<_> = stream.collect().await;
    let samples: Vec<_> = events
        .iter()
        .filter_map(|event| match event.payload {
            RuntimeJournalPayload::ModelOutputRateUpdated { rate } => Some(rate),
            _ => None,
        })
        .collect();
    assert!(
        samples.len() <= 3,
        "replaceable rate updates must not accumulate a backlog: {samples:?}"
    );
    assert_eq!(samples.last().unwrap().unwrap().output_tokens(), 2_400);
    assert!(matches!(
        events.last().unwrap().payload,
        RuntimeJournalPayload::StepCompleted
    ));
}

#[tokio::test]
async fn output_rate_uses_complete_provider_usage_and_the_same_receive_interval() {
    let usage = Usage::with_details(20_000, None, 2_400, Some(2_000), 22_400);
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![Ok(progress()), Ok(completed(Some(usage)))]),
        ScriptedModelProviderResponse::Stream(vec![Ok(completed(Some(usage)))]),
    ]);
    let runtime = Runtime::builder(session_id("output-rate-usage"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .unwrap();
    let observed = rates(&runtime).await;
    assert_eq!(observed.len(), 1, "rate snapshots are latest-only");
    assert_eq!(
        observed[0],
        Some(ModelOutputRate::new(
            2_400,
            Duration::from_secs(8),
            merry_core::OutputTokenSource::ProviderUsage
        ))
    );
    assert_eq!(observed[0].unwrap().tokens_per_second(), Some(300.0));
    assert_eq!(
        rates(&runtime).await,
        vec![None],
        "an untimed next request cannot reuse the previous sample"
    );
}

#[tokio::test]
async fn output_rate_keeps_estimates_when_usage_is_missing() {
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![Ok(progress()), Ok(completed(None))]),
    ]);
    let runtime = Runtime::builder(session_id("output-rate-no-usage"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .unwrap();
    let observed = rates(&runtime).await;
    assert_eq!(
        observed.last(),
        Some(&Some(ModelOutputRate::new(
            400,
            Duration::from_secs(8),
            merry_core::OutputTokenSource::Estimated
        )))
    );
}

#[tokio::test]
async fn cancelled_output_does_not_invent_a_completion_time_or_actual_usage() {
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![Ok(progress()), Err(ModelError::Cancelled)]),
    ]);
    let runtime = Runtime::builder(session_id("output-rate-cancelled"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .unwrap();
    assert_eq!(
        rates(&runtime).await,
        vec![Some(ModelOutputRate::new(
            400,
            Duration::from_secs(8),
            merry_core::OutputTokenSource::Estimated
        ))]
    );
}

#[tokio::test]
async fn retry_reset_cannot_pair_previous_attempt_timing_with_new_usage() {
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![
            Ok(progress()),
            Ok(ModelEvent::OutputProgress { progress: None }),
            Ok(completed(Some(Usage::new(100, 500)))),
        ])]);
    let runtime = Runtime::builder(session_id("output-rate-reset"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .unwrap();
    assert_eq!(rates(&runtime).await, vec![None]);
}

#[tokio::test]
async fn provider_usage_does_not_hide_partial_or_consumer_limited_timing() {
    use merry_core::{OutputTimingQuality, OutputTokenSource};
    for quality in [
        OutputTimingQuality::ReceiveWindow,
        OutputTimingQuality::PartialOutput,
        OutputTimingQuality::ConsumerLimited,
    ] {
        let sample =
            ModelOutputProgress::new(400, Duration::from_secs(2)).with_timing_quality(quality);
        let provider =
            RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![
                Ok(ModelEvent::OutputProgress {
                    progress: Some(sample),
                }),
                Ok(completed(Some(Usage::with_details(
                    10,
                    None,
                    2_400,
                    Some(2_000),
                    2_410,
                )))),
            ])]);
        let runtime = Runtime::builder(session_id("output-rate-quality"))
            .model_provider(Arc::new(provider), model_name())
            .build()
            .unwrap();
        let observed = rates(&runtime).await;
        let final_rate = observed.last().unwrap().unwrap();
        assert_eq!(final_rate.token_source(), OutputTokenSource::ProviderUsage);
        assert_eq!(final_rate.output_tokens(), 2_400);
        assert!(final_rate.is_estimated());
        assert_eq!(final_rate.tokens_per_second(), Some(1_200.0));
        if quality == OutputTimingQuality::ConsumerLimited {
            assert_eq!(
                final_rate.timing_quality(),
                OutputTimingQuality::ConsumerLimited
            );
        } else {
            assert_eq!(
                final_rate.timing_quality(),
                OutputTimingQuality::PartialOutput
            );
        }
    }
}
