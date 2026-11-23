use super::support::*;
use crate::{MCP_CALL_TOOL, McpServerConfig, discover_mcp_tools};
use merry_core::{SessionId, ToolCallResultStatus, ToolOutput};
use merry_llm::ModelName;
use merry_runtime::{FileSessionStore, Runtime, ToolExecutionContext};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn newly_added_server_tools_are_discovered_and_callable_mid_session() {
    let server = Server::start(Mode::Online, vec![tool("read")]).await;
    let report = discover_mcp_tools(&[server.config("docs")]).await.unwrap();

    // Server deploys a brand-new tool mid-session without restarting Merry.
    let mut deploy_tool = tool("deploy_preview");
    deploy_tool["description"] = json!("Deploy a documentation preview branch");
    deploy_tool["inputSchema"] = json!({
        "type": "object",
        "properties": { "branch": { "type": "string" } },
        "required": ["branch"]
    });
    server.set_tools(vec![tool("read"), deploy_tool]);

    let provider = sequence_provider(vec![
        mcp_search_call(json!({"query": "deploy preview", "refresh": true})),
        (
            MCP_CALL_TOOL,
            mcp_call_args("docs", "deploy_preview", json!({"branch": "main"})),
        ),
    ]);
    let rt = runtime("mid-session-new-tool", report, &provider);

    let search_call = next_call(&rt).await;
    let search_events = rt
        .execute_tool_call(
            &search_call,
            ToolExecutionContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    let search_result = resolved(&search_events);
    assert_eq!(search_result.status(), ToolCallResultStatus::Succeeded);
    let payload = read_json_output(&rt, search_result).await;
    assert_eq!(payload["tools"][0]["server"], "docs");
    assert_eq!(payload["tools"][0]["tool"], "deploy_preview");
    assert_eq!(
        payload["tools"][0]["inputSchema"]["required"],
        json!(["branch"])
    );

    let invoke_call = next_call(&rt).await;
    let invoke_events = rt
        .execute_tool_call(
            &invoke_call,
            ToolExecutionContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    let invoke_result = resolved(&invoke_events);
    assert_eq!(invoke_result.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        rt.read_tool_output(invoke_result.call_id()).await.unwrap(),
        Some(ToolOutput::text("done:deploy_preview"))
    );

    // Provider-visible tool specs and stable prefix hash never change mid-session.
    let requests = provider.recorded_requests();
    assert_eq!(requests[0].tools(), requests[1].tools());
    assert_eq!(
        requests[0].stable_prefix_hash(),
        requests[1].stable_prefix_hash()
    );
    server.stop().await;
}

#[tokio::test]
async fn direct_mcp_call_refreshes_lazily_when_new_tool_is_not_yet_cached() {
    let server = Server::start(Mode::Online, vec![tool("read")]).await;
    let report = discover_mcp_tools(&[server.config("docs")]).await.unwrap();
    let initial_lists = server.list_calls();

    // Server adds `summarize` silently without sending a notification.
    server.set_tools(vec![tool("read"), tool("summarize")]);

    let rt = runtime(
        "lazy-refresh-on-call",
        report,
        &mcp_call_provider("docs", "summarize", 1),
    );
    let call = next_call(&rt).await;
    let events = rt
        .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(resolved(&events).status(), ToolCallResultStatus::Succeeded);
    assert_eq!(server.list_calls(), initial_lists + 1);
    assert_eq!(server.calls(), 1);
    server.stop().await;
}

#[tokio::test]
async fn notifications_tools_list_changed_triggers_automatic_refresh_without_explicit_flag() {
    let server = Server::start(Mode::SseNotifyToolsChanged, vec![tool("trigger")]).await;
    let report = discover_mcp_tools(&[server.config("live")]).await.unwrap();
    let lists_after_startup = server.list_calls();

    // Server updates its tool list, then pushes `notifications/tools/list_changed`
    // inside the SSE response stream for `trigger`.
    server.set_tools(vec![tool("trigger"), tool("brand_new")]);

    let provider = sequence_provider(vec![
        (MCP_CALL_TOOL, mcp_call_args("live", "trigger", json!({}))),
        mcp_search_call(json!({"query": "brand_new"})),
    ]);
    let rt = runtime("list-changed-notification", report, &provider);

    let trigger_call = next_call(&rt).await;
    let trigger_events = rt
        .execute_tool_call(
            &trigger_call,
            ToolExecutionContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    assert_eq!(
        resolved(&trigger_events).status(),
        ToolCallResultStatus::Succeeded
    );

    // Search WITHOUT refresh=true must still automatically re-fetch because
    // ClientHandler::on_tool_list_changed marked the server dirty.
    let search_call = next_call(&rt).await;
    let search_events = rt
        .execute_tool_call(
            &search_call,
            ToolExecutionContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    let payload = read_json_output(&rt, resolved(&search_events)).await;
    assert_eq!(payload["tools"][0]["tool"], "brand_new");
    assert_eq!(server.list_calls(), lists_after_startup + 1);
    server.stop().await;
}

#[tokio::test]
async fn schema_validation_failure_returns_live_input_schema_without_calling_server() {
    let mut strict_tool = tool("query_db");
    strict_tool["inputSchema"] = json!({
        "type": "object",
        "properties": { "sql": { "type": "string" } },
        "required": ["sql"]
    });
    let server = Server::start(Mode::Online, vec![strict_tool]).await;
    let report = discover_mcp_tools(&[server.config("db")]).await.unwrap();

    let provider = sequence_provider(vec![
        (MCP_CALL_TOOL, mcp_call_args("db", "query_db", json!({}))),
        (
            MCP_CALL_TOOL,
            mcp_call_args("db", "query_db", json!({"sql": "SELECT 1"})),
        ),
    ]);
    let rt = runtime("schema-validation", report, &provider);

    let bad_call = next_call(&rt).await;
    let bad_events = rt
        .execute_tool_call(
            &bad_call,
            ToolExecutionContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    let bad_result = resolved(&bad_events);
    assert_eq!(bad_result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        bad_result.diagnostic().unwrap().code(),
        "mcp_invalid_arguments"
    );
    let error_payload = read_json_output(&rt, bad_result).await;
    assert_eq!(error_payload["inputSchema"]["required"], json!(["sql"]));
    assert_eq!(server.calls(), 0);

    let good_call = next_call(&rt).await;
    let good_events = rt
        .execute_tool_call(
            &good_call,
            ToolExecutionContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    assert_eq!(
        resolved(&good_events).status(),
        ToolCallResultStatus::Succeeded
    );
    assert_eq!(server.calls(), 1);
    server.stop().await;
}

#[tokio::test]
async fn silent_mid_session_schema_update_is_refreshed_on_validation_mismatch() {
    let mut old_tool = tool("query_db");
    old_tool["inputSchema"] = json!({
        "type": "object",
        "properties": { "q": { "type": "string" } },
        "required": ["q"]
    });
    let server = Server::start(Mode::Online, vec![old_tool]).await;
    let report = discover_mcp_tools(&[server.config("db")]).await.unwrap();

    // Server silently evolves the tool's schema from `q` to `sql` without pushing a notification.
    let mut new_tool = tool("query_db");
    new_tool["inputSchema"] = json!({
        "type": "object",
        "properties": { "sql": { "type": "string" } },
        "required": ["sql"]
    });
    server.set_tools(vec![new_tool]);

    let rt = runtime(
        "silent-schema-evolution",
        report,
        &sequence_provider(vec![(
            MCP_CALL_TOOL,
            mcp_call_args("db", "query_db", json!({"sql": "SELECT 1"})),
        )]),
    );
    let call = next_call(&rt).await;
    let events = rt
        .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(resolved(&events).status(), ToolCallResultStatus::Succeeded);
    assert_eq!(server.calls(), 1);
    server.stop().await;
}

#[tokio::test]
async fn explicit_refresh_reconnects_server_that_started_offline_or_auth_failed() {
    let server = Server::start(Mode::Status(401), vec![tool("secret_lookup")]).await;
    let report = discover_mcp_tools(&[server.config("vault")]).await.unwrap();
    assert_eq!(report.diagnostics().len(), 1);

    // Server comes back healthy; explicit `refresh: true` reconnects immediately.
    server.set_mode(Mode::Online);
    let provider = sequence_provider(vec![
        mcp_search_call(json!({"server": "vault", "refresh": true})),
        (
            MCP_CALL_TOOL,
            mcp_call_args("vault", "secret_lookup", json!({})),
        ),
    ]);
    let rt = runtime("explicit-refresh-recovery", report, &provider);

    let search_call = next_call(&rt).await;
    let search_events = rt
        .execute_tool_call(
            &search_call,
            ToolExecutionContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    let payload = read_json_output(&rt, resolved(&search_events)).await;
    assert_eq!(payload["servers"][0]["status"], "ready");
    assert_eq!(payload["tools"][0]["tool"], "secret_lookup");

    let call = next_call(&rt).await;
    let events = rt
        .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(resolved(&events).status(), ToolCallResultStatus::Succeeded);
    assert_eq!(server.calls(), 1);
    server.stop().await;
}

#[tokio::test]
async fn allowlist_hides_and_blocks_disallowed_tools() {
    let server = Server::start(Mode::Online, vec![tool("allowed"), tool("forbidden")]).await;
    let config = McpServerConfig::builder("docs", server.url())
        .tools(["allowed"])
        .build();
    let report = discover_mcp_tools(&[config]).await.unwrap();

    let provider = sequence_provider(vec![
        mcp_search_call(json!({"server": "docs"})),
        (MCP_CALL_TOOL, mcp_call_args("docs", "forbidden", json!({}))),
    ]);
    let rt = runtime("allowlist-enforcement", report, &provider);

    let search_call = next_call(&rt).await;
    let search_events = rt
        .execute_tool_call(
            &search_call,
            ToolExecutionContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    let payload = read_json_output(&rt, resolved(&search_events)).await;
    assert_eq!(payload["servers"][0]["tools"], json!(["allowed"]));

    let blocked_call = next_call(&rt).await;
    let blocked_events = rt
        .execute_tool_call(
            &blocked_call,
            ToolExecutionContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    let result = resolved(&blocked_events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(result.diagnostic().unwrap().code(), "mcp_tool_disallowed");
    assert_eq!(server.calls(), 0);
    server.stop().await;
}

#[tokio::test]
async fn resuming_session_preserves_stable_prefix_even_when_remote_server_changes() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(temp.path());
    let id = SessionId::new("dynamic-resume").unwrap();
    let server = Server::start(Mode::Online, vec![tool("alpha")]).await;
    let config = server.config("docs");
    let provider = text_provider();

    let original = runtime(
        id.as_str(),
        discover_mcp_tools(std::slice::from_ref(&config))
            .await
            .unwrap(),
        &provider,
    );
    step(&original).await;
    original.save_session_to(store.clone()).await.unwrap();

    // Remote server adds tools and changes schemas before resume.
    server.set_tools(vec![tool("alpha"), tool("beta"), tool("gamma")]);
    let resumed_discovery = discover_mcp_tools(std::slice::from_ref(&config))
        .await
        .unwrap();
    let builder = Runtime::builder(id).fully_trusted_tools().model_provider(
        Arc::new(provider.clone()),
        ModelName::new("fixture").unwrap(),
    );
    let resumed = resumed_discovery
        .into_parts()
        .0
        .into_iter()
        .fold(builder, |builder, registration| {
            builder.register_tool(registration.tool)
        })
        .resume_from_store(store)
        .await
        .unwrap();
    step(&resumed).await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools(), requests[1].tools());
    assert_eq!(
        requests[0].stable_prefix_hash(),
        requests[1].stable_prefix_hash()
    );
    server.stop().await;
}
