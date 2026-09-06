use crate::{agent, text_provider};
use merry::{AgentLoopStatus, RuntimeEvent};

#[tokio::test]
async fn run_returns_public_events_and_terminal_result() {
    let agent = agent(text_provider("hello"));

    let result = agent.run("say hello").await.expect("run should complete");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("hello"));
    assert!(result.events().iter().any(|event| matches!(
        event,
        RuntimeEvent::AssistantMessage { text, .. } if text == "hello"
    )));
}

#[tokio::test]
async fn stream_result_projects_the_same_public_contract() {
    let agent = agent(text_provider("streamed"));
    let mut stream = agent.stream("say streamed").expect("stream should start");
    let mut events = Vec::new();

    while let Some(event) = stream.next().await.expect("driver should advance") {
        events.push(event);
    }
    let result = stream
        .result()
        .await
        .expect("stream result should complete");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("streamed"));
    assert_eq!(events, result.events());
}
