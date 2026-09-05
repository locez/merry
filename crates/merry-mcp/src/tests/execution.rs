use super::support::*;
use crate::{discover_mcp_tools, restore_mcp_tools};
use merry_core::ToolCallResultStatus;
use merry_runtime::{FileSessionStore, RuntimeError, ToolExecutionContext};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn offline_tools_resolve_failure_then_reconnect_without_changing_definitions() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(temp.path());
    let server = Server::start(Mode::Online, vec![tool("read")]).await;
    let config = server.config("server");
    let original = runtime(
        "online",
        discover_mcp_tools(std::slice::from_ref(&config))
            .await
            .unwrap(),
        &text_provider(),
    );
    let catalog = original.external_tool_catalog().await;
    server.set_mode(Mode::Status(503));
    let report = restore_mcp_tools(&[config], &catalog).await.unwrap();
    let provider = call_provider(catalog.entries()[0].spec().name(), 2);
    let runtime = runtime("recover", report, &provider);
    let call = next_call(&runtime).await;
    let events = runtime
        .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
        .await
        .unwrap();
    let result = resolved(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result.diagnostic().unwrap().code(),
        "mcp_server_unavailable"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("failed tool result has a readable durable artifact");
    runtime.save_session_to(store).await.unwrap();
    assert_eq!(server.calls(), 0);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::time::resume();
    server.set_mode(Mode::Online);
    let call = next_call(&runtime).await;
    let events = runtime
        .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(resolved(&events).status(), ToolCallResultStatus::Succeeded);
    assert_eq!(server.calls(), 1);
    let requests = provider.recorded_requests();
    assert_eq!(requests[0].tools(), requests[1].tools());
    assert_eq!(
        requests[0].stable_prefix_hash(),
        requests[1].stable_prefix_hash()
    );
    server.stop().await;
}

#[tokio::test]
async fn authentication_failure_is_not_retried_on_each_tool_call() {
    let server = Server::start(Mode::Online, vec![tool("read")]).await;
    let original = runtime(
        "auth-origin",
        discover_mcp_tools(&[server.config("server")])
            .await
            .unwrap(),
        &text_provider(),
    );
    let catalog = original.external_tool_catalog().await;
    server.set_mode(Mode::Status(401));
    let report = restore_mcp_tools(&[server.config("server")], &catalog)
        .await
        .unwrap();
    let requests_before = server.requests();
    server.set_mode(Mode::Online);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::time::resume();
    let runtime = runtime(
        "auth-blocked",
        report,
        &call_provider(catalog.entries()[0].spec().name(), 2),
    );
    for _attempt in 0..2 {
        let call = next_call(&runtime).await;
        let events = runtime
            .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
            .await
            .unwrap();
        assert_eq!(
            resolved(&events).diagnostic().unwrap().code(),
            "mcp_server_unavailable"
        );
    }
    assert_eq!(server.requests(), requests_before);
    server.stop().await;
}

#[tokio::test]
async fn expired_mcp_session_reconnects_without_replaying_the_failed_call() {
    let server = Server::start(Mode::Online, vec![tool("read")]).await;
    let config = server.config("server");
    let report = discover_mcp_tools(std::slice::from_ref(&config))
        .await
        .unwrap();
    let name = report.registrations()[0].tool.spec().name().clone();
    let provider = call_provider(&name, 2);
    let runtime = runtime("expired-session", report, &provider);
    server.expire_session();

    let call = next_call(&runtime).await;
    let events = runtime
        .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(
        resolved(&events).diagnostic().unwrap().code(),
        "mcp_server_unavailable"
    );
    assert_eq!(server.calls(), 1);

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::time::resume();
    let call = next_call(&runtime).await;
    let events = runtime
        .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(resolved(&events).status(), ToolCallResultStatus::Succeeded);
    assert_eq!(server.calls(), 2);
    server.stop().await;
}

#[tokio::test]
async fn call_timeout_records_unknown_outcome_without_replaying_side_effects() {
    let mut server = Server::start(Mode::Online, vec![tool("write")]).await;
    let report = discover_mcp_tools(&[server.config("server")])
        .await
        .unwrap();
    let name = report.registrations()[0].tool.spec().name().clone();
    let runtime = runtime("unknown-outcome", report, &call_provider(&name, 1));
    let call = next_call(&runtime).await;
    server.set_mode(Mode::StallCall);
    let owned_runtime = runtime.clone();
    let owned_call = call.clone();
    let execution = tokio::spawn(async move {
        owned_runtime
            .execute_tool_call(
                &owned_call,
                ToolExecutionContext::new(CancellationToken::new()),
            )
            .await
    });
    server.wait_for("tools/call").await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::time::resume();
    let events = execution.await.unwrap().unwrap();
    assert_eq!(
        resolved(&events).diagnostic().unwrap().code(),
        "mcp_tool_outcome_unknown"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert!(
        runtime
            .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
            .await
            .is_err()
    );
    assert_eq!(server.calls(), 1);
    server.stop().await;
}

#[tokio::test]
async fn cancellation_propagates_and_stops_in_flight_tool_execution() {
    let mut server = Server::start(Mode::Online, vec![tool("write")]).await;
    let report = discover_mcp_tools(&[server.config("server")])
        .await
        .unwrap();
    let name = report.registrations()[0].tool.spec().name().clone();
    let runtime = runtime("cancel-call", report, &call_provider(&name, 1));
    let call = next_call(&runtime).await;
    server.set_mode(Mode::StallCall);
    let token = CancellationToken::new();
    let owned_token = token.clone();
    let owned_runtime = runtime.clone();
    let execution = tokio::spawn(async move {
        owned_runtime
            .execute_tool_call(&call, ToolExecutionContext::new(owned_token))
            .await
    });
    server.wait_for("tools/call").await;
    token.cancel();
    assert!(matches!(
        execution.await.unwrap(),
        Err(RuntimeError::ToolExecutionCancelled { .. })
    ));
    assert_eq!(server.calls(), 1);
    assert_eq!(runtime.pending_tool_calls().await.len(), 1);
    runtime
        .abandon_pending_tool_calls("test cancellation settled")
        .await
        .unwrap();
    server.stop().await;
}
