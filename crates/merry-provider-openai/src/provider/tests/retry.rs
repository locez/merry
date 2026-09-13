use super::model_request;
use crate::{OpenAiProviderConfig, parse::ResponsesStreamParser};
use futures_util::{StreamExt, stream};
use merry_core::ProviderName;
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelProvider,
    ModelProviderFuture, ModelRequest, ModelRetryPolicy, ModelStreamContext, ProviderErrorKind,
    RetryingModelProvider,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

const SERVER_ERROR: &str =
    r#"data: {"type":"error","code":"server_error","message":"Please try again later."}"#;
const FAILED_SERVER_ERROR: &str = r#"data: {"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"Please try again later."}}}"#;
const RATE_LIMIT: &str =
    r#"data: {"type":"error","code":"rate_limit_exceeded","message":"Too many requests."}"#;
const FAILED_RATE_LIMIT: &str = r#"data: {"type":"response.failed","response":{"status":"failed","error":{"code":"rate_limit_exceeded","message":"Too many requests."}}}"#;
const SUCCESS: &str = r#"data: {"type":"response.output_text.delta","delta":"Recovered"}
data: {"type":"response.completed","response":{"status":"completed"}}"#;

/// Feeds wire fixtures through the real Responses parser and public retry wrapper.
struct ScriptedResponsesProvider {
    config: OpenAiProviderConfig,
    scripts: Mutex<VecDeque<String>>,
}

impl ScriptedResponsesProvider {
    fn new(scripts: Vec<String>) -> Self {
        Self {
            config: OpenAiProviderConfig::new("sk-test").expect("valid fixture config"),
            scripts: Mutex::new(scripts.into()),
        }
    }

    fn scripts_remaining(&self) -> usize {
        self.scripts.lock().expect("fixture lock").len()
    }
}

impl ModelProvider for ScriptedResponsesProvider {
    fn name(&self) -> &ProviderName {
        self.config.provider_name()
    }

    fn capabilities(&self) -> &ModelCapabilities {
        self.config.capabilities()
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            let script = self
                .scripts
                .lock()
                .expect("fixture lock")
                .pop_front()
                .expect("retry must stay within scripted attempts");
            let mut parser = ResponsesStreamParser::new();
            let mut events = vec![Ok(ModelEvent::Started)];
            for line in script.lines() {
                match parser.parse_sse_line(line) {
                    Ok(parsed) => events.extend(parsed.into_iter().map(Ok)),
                    Err(error) => {
                        events.push(Err(error.into()));
                        break;
                    }
                }
            }
            if events.last().is_some_and(Result::is_ok)
                && let Err(error) = parser.finish()
            {
                events.push(Err(error.into()));
            }
            let events: ModelEventStream = Box::pin(stream::iter(events));
            Ok(events)
        })
    }
}

async fn run_scripts(
    scripts: Vec<String>,
    policy: ModelRetryPolicy,
) -> (Vec<Result<ModelEvent, ModelError>>, usize) {
    let attempts = scripts.len();
    let inner = Arc::new(ScriptedResponsesProvider::new(scripts));
    let provider = RetryingModelProvider::new(inner.clone(), policy);
    let events = tokio::time::timeout(Duration::from_secs(5), async {
        provider
            .stream_model(model_request(), ModelStreamContext::default())
            .await
            .expect("retry stream setup")
            .collect::<Vec<_>>()
            .await
    })
    .await
    .expect("scripted retry must finish within timeout");
    (events, attempts - inner.scripts_remaining())
}

fn retry_policy() -> ModelRetryPolicy {
    ModelRetryPolicy::new(
        true,
        2,
        Duration::from_millis(1),
        Duration::from_millis(1),
        Duration::from_secs(5),
        false,
    )
    .expect("valid fixture policy")
}

#[tokio::test]
async fn responses_transient_errors_retry_before_output() {
    for failure in [
        SERVER_ERROR,
        FAILED_SERVER_ERROR,
        RATE_LIMIT,
        FAILED_RATE_LIMIT,
    ] {
        let (events, attempts) =
            run_scripts(vec![failure.to_owned(), SUCCESS.to_owned()], retry_policy()).await;
        let events = events
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("transient failure should recover");
        assert_eq!(attempts, 2);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], ModelEvent::Started);
        assert_eq!(
            events[1],
            ModelEvent::OutputTextDelta {
                delta: "Recovered".to_owned()
            }
        );
        assert!(
            matches!(&events[2], ModelEvent::Completed { response } if response.finish_reason() == FinishReason::Stop)
        );
    }
}

#[tokio::test]
async fn responses_server_errors_respect_disabled_and_exhausted_retry_budgets() {
    for (policy, expected_attempts) in [(ModelRetryPolicy::disabled(), 1), (retry_policy(), 2)] {
        let (events, attempts) = run_scripts(vec![FAILED_SERVER_ERROR.to_owned(); 3], policy).await;
        assert_eq!(attempts, expected_attempts);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[1], Err(error) if error.kind() == ProviderErrorKind::Unavailable));
    }
}

#[tokio::test]
async fn responses_server_error_does_not_retry_after_text_or_tool_output() {
    for prefix in [
        r#"data: {"type":"response.output_text.delta","delta":"Partial"}"#,
        r#"data: {"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"read_file","arguments":"{}"}}"#,
    ] {
        for failure in [SERVER_ERROR, FAILED_SERVER_ERROR] {
            let (events, attempts) = run_scripts(
                vec![format!("{prefix}\n{failure}"), SUCCESS.to_owned()],
                retry_policy(),
            )
            .await;
            assert_eq!(attempts, 1);
            assert_eq!(events.len(), 3);
            assert!(matches!(
                &events[1],
                Ok(ModelEvent::OutputTextDelta { .. } | ModelEvent::ToolCallRequested { .. })
            ));
            assert!(
                matches!(&events[2], Err(error) if error.kind() == ProviderErrorKind::Unavailable)
            );
        }
    }
}

#[tokio::test]
async fn responses_unknown_or_missing_error_codes_do_not_retry() {
    for failure in [
        r#"data: {"type":"error","code":"invalid_request_error","message":"Please try again later."}"#,
        r#"data: {"type":"error","code":"unknown","message":"server_error"}"#,
        r#"data: {"type":"error","message":"server_error"}"#,
        r#"data: {"type":"response.failed","response":{"status":"failed","error":{"code":"invalid_prompt","message":"Please try again later."}}}"#,
        r#"data: {"type":"response.failed","response":{"status":"failed","error":{"message":"server_error"}}}"#,
        r#"data: {"type":"response.failed","response":{"status":"failed","error":null}}"#,
    ] {
        let (events, attempts) =
            run_scripts(vec![failure.to_owned(), SUCCESS.to_owned()], retry_policy()).await;
        assert_eq!(attempts, 1);
        assert_eq!(events.len(), 2);
        match &events[1] {
            Err(error) => assert_eq!(error.kind(), ProviderErrorKind::Protocol),
            Ok(ModelEvent::Completed { response }) => {
                assert_eq!(response.finish_reason(), FinishReason::Error)
            }
            other => panic!("expected terminal failure, got {other:?}"),
        }
    }
}
