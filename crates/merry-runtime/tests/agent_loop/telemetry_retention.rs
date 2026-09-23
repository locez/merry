use crate::support::{
    models::{ScriptedModelProvider, completed_text_event},
    runtime::{run_default_loop, runtime_with_provider},
};
use merry_core::RuntimeEvent;
use merry_llm::{ModelError, ModelEvent, ModelOutputProgress};
use merry_runtime::{AgentLoopConfig, AgentRunMessage, StepContext, StepInput};
use std::time::Duration;

fn long_output(fragments: u64) -> Vec<Result<ModelEvent, ModelError>> {
    (0..fragments)
        .flat_map(|index| {
            [
                Ok(ModelEvent::OutputProgress {
                    progress: Some(ModelOutputProgress::new(
                        index * 4,
                        Duration::from_millis(index),
                    )),
                }),
                Ok(ModelEvent::OutputTextDelta {
                    delta: "x".to_owned(),
                }),
            ]
        })
        .chain([Ok(completed_text_event("done"))])
        .collect()
}

#[tokio::test]
async fn retained_run_evidence_does_not_grow_with_transient_output_fragments() {
    let mut retained_count = None;
    for fragments in [1, 10_000] {
        let runtime = runtime_with_provider(
            "bounded-retained-telemetry",
            ScriptedModelProvider::new(vec![long_output(fragments)]),
        );
        let result = run_default_loop(&runtime, "finish").await;
        assert_eq!(result.final_output(), Some("done"));
        assert!(
            result
                .events()
                .iter()
                .all(|event| !event.payload.is_transient())
        );
        if let Some(count) = retained_count {
            assert_eq!(result.events().len(), count);
        }
        retained_count = Some(result.events().len());
    }
}

#[tokio::test]
async fn live_output_is_forwarded_without_entering_the_stream_result() {
    let runtime = runtime_with_provider(
        "bounded-stream-telemetry",
        ScriptedModelProvider::new(vec![long_output(10_000)]),
    );
    let mut run = runtime
        .run_agent_loop_stream(
            StepInput::user_text("finish").unwrap(),
            StepContext::default(),
            AgentLoopConfig::default(),
        )
        .unwrap();
    let mut deltas = 0;
    let mut samples = 0;
    while let Some(message) = run.next_message().await.unwrap() {
        match message {
            AgentRunMessage::Event(RuntimeEvent::AssistantMessageDelta { .. }) => deltas += 1,
            AgentRunMessage::Event(RuntimeEvent::ModelOutputRateUpdated {
                rate: Some(_), ..
            }) => samples += 1,
            _ => {}
        }
    }
    let result = run.result().await.unwrap();
    assert_eq!(deltas, 10_000);
    assert!(samples > 0);
    assert_eq!(result.final_output(), Some("done"));
    assert!(
        result
            .events()
            .iter()
            .all(|event| !event.payload.is_transient())
    );
    assert!(result.events().len() < 20);
}
