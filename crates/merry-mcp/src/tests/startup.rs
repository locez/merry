use super::support::*;
use crate::{
    MCP_CALL_TOOL, MCP_SEARCH_TOOL, McpError, McpFailureKind, McpServerConfig, McpServerIssue,
    discover_mcp_tools,
};
use merry_core::{ToolCallResultStatus, ToolOutput};
use merry_runtime::ToolExecutionContext;
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn empty_configuration_registers_no_tools_or_diagnostics() {
    let report = discover_mcp_tools(&[]).await.unwrap();
    assert!(report.registrations().is_empty());
    assert!(report.diagnostics().is_empty());
}

#[tokio::test]
async fn partial_and_total_outages_are_nonfatal_and_keep_meta_tools_available() {
    let healthy = Server::start(Mode::Online, vec![tool("zeta"), tool("alpha")]).await;
    let offline = Server::start(Mode::Status(503), vec![]).await;
    let report = discover_mcp_tools(&[offline.config("offline"), healthy.config("healthy")])
        .await
        .unwrap();
    assert_eq!(
        report
            .registrations()
            .iter()
            .map(|registration| registration.tool.spec().name().as_str())
            .collect::<Vec<_>>(),
        [MCP_SEARCH_TOOL, MCP_CALL_TOOL]
    );
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].server_id().as_str(), "offline");

    let provider = sequence_provider(vec![mcp_search_call(json!({}))]);
    let rt = runtime("partial-outage", report, &provider);
    let call = next_call(&rt).await;
    let events = rt
        .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
        .await
        .unwrap();
    let payload = read_json_output(&rt, resolved(&events)).await;
    assert_eq!(payload["servers"][0]["server"], "healthy");
    assert_eq!(payload["servers"][0]["status"], "ready");
    assert_eq!(payload["servers"][0]["tools"], json!(["alpha", "zeta"]));
    assert_eq!(payload["servers"][1]["server"], "offline");
    assert_eq!(payload["servers"][1]["status"], "unavailable");

    let total_outage = discover_mcp_tools(&[offline.config("offline")])
        .await
        .unwrap();
    // Meta-tools stay registered so the session can recover once `offline` comes back online.
    assert_eq!(total_outage.registrations().len(), 2);
    assert!(matches!(
        total_outage.diagnostics()[0].issue(),
        McpServerIssue::Unavailable {
            failure: McpFailureKind::Http(503),
            ..
        }
    ));
    healthy.stop().await;
    offline.stop().await;
}

#[tokio::test]
async fn meta_tool_specs_are_stable_regardless_of_config_or_page_order() {
    let first = Server::start(Mode::Paginated, vec![tool("zeta"), tool("alpha")]).await;
    let second = Server::start(Mode::Online, vec![tool("lookup")]).await;
    let before = discover_mcp_tools(&[second.config("second"), first.config("first")])
        .await
        .unwrap();
    first.set_tools(vec![tool("alpha"), tool("zeta")]);
    let after = discover_mcp_tools(&[first.config("first"), second.config("second")])
        .await
        .unwrap();
    let specs = |report: &crate::McpDiscovery| {
        report
            .registrations()
            .iter()
            .map(|registration| registration.tool.spec().clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(specs(&before), specs(&after));
    assert_eq!(before.registrations().len(), 2);
    assert!(before.diagnostics().is_empty());
    first.stop().await;
    second.stop().await;
}

#[tokio::test]
async fn invalid_configuration_is_fatal_before_any_server_is_contacted() {
    let healthy = Server::start(Mode::Online, vec![tool("read")]).await;
    let bad = McpServerConfig::builder("bad", "not-an-endpoint?secret-query-value").build();
    let error = discover_mcp_tools(&[healthy.config("first"), bad])
        .await
        .unwrap_err();
    assert!(matches!(error, McpError::InvalidConfiguration { .. }));
    assert!(!error.to_string().contains("secret-query-value"));
    assert_eq!(healthy.requests(), 0);
    let bad = McpServerConfig::builder("bad", healthy.url())
        .header("Authorization", "secret-query-value\ninvalid")
        .build();
    assert!(discover_mcp_tools(&[bad]).await.is_err());
    for reserved in ["Accept", "Last-Event-Id"] {
        let bad = McpServerConfig::builder("bad", healthy.url())
            .header(reserved, "override")
            .build();
        assert!(matches!(
            discover_mcp_tools(&[bad]).await,
            Err(McpError::InvalidConfiguration { .. })
        ));
    }
    assert_eq!(healthy.requests(), 0);
    healthy.stop().await;
}

#[tokio::test]
async fn stalled_startup_times_out_without_discarding_other_servers() {
    let mut stalled = Server::start(Mode::StallInitialize, vec![]).await;
    let healthy = Server::start(Mode::Online, vec![tool("lookup")]).await;
    let configs = vec![
        stalled.config("a"),
        stalled.config("b"),
        stalled.config("c"),
        stalled.config("d"),
        healthy.config("z-healthy"),
    ];
    let discovery = tokio::spawn(async move { discover_mcp_tools(&configs).await });
    for _request in 0..4 {
        stalled.wait_for("initialize").await;
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::time::resume();
    let report = discovery.await.unwrap().unwrap();
    assert_eq!(report.registrations().len(), 2);
    assert_eq!(report.diagnostics().len(), 4);
    assert!(matches!(
        report.diagnostics()[0].issue(),
        McpServerIssue::Unavailable {
            failure: McpFailureKind::Timeout,
            ..
        }
    ));
    stalled.stop().await;
    healthy.stop().await;
}

#[tokio::test]
async fn dropping_discovery_stops_the_handshake_without_detached_producers() {
    let mut server = Server::start(Mode::StallInitialize, vec![]).await;
    let config = server.config("cancelled");
    let discovery = tokio::spawn(async move { discover_mcp_tools(&[config]).await });
    server.wait_for("initialize").await;
    let requests_at_abort = server.requests();
    discovery.abort();
    assert!(discovery.await.unwrap_err().is_cancelled());
    // Auto mode sends `server/discover` then `initialize`; no further requests arrive after abort.
    assert_eq!(requests_at_abort, 2);
    assert_eq!(server.requests(), requests_at_abort);
    server.stop().await;
}

#[tokio::test]
async fn the_total_startup_budget_prevents_contacting_queued_servers_after_expiry() {
    let mut server = Server::start(Mode::StallInitialize, vec![]).await;
    let configs = (0..12)
        .map(|index| server.config(&format!("server-{index:02}")))
        .collect::<Vec<_>>();
    let discovery = tokio::spawn(async move { discover_mcp_tools(&configs).await });
    for _request in 0..4 {
        server.wait_for("initialize").await;
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::time::resume();
    let report = discovery.await.unwrap().unwrap();
    assert_eq!(report.registrations().len(), 2);
    assert_eq!(report.diagnostics().len(), 12);
    // Only the first 4 concurrent slots ran (`server/discover` + `initialize` each);
    // the remaining 8 queued servers were never contacted.
    assert_eq!(server.requests(), 8);
    server.stop().await;
}

#[tokio::test]
async fn untrusted_remote_failures_are_bounded_and_diagnostics_are_redacted() {
    for (mode, expected) in [
        (Mode::InvalidJson, McpFailureKind::Protocol),
        (Mode::RpcSecret, McpFailureKind::Protocol),
        (Mode::Status(401), McpFailureKind::Authentication),
        (Mode::Oversized, McpFailureKind::ResponseTooLarge),
    ] {
        let server = Server::start(mode, vec![]).await;
        let config = McpServerConfig::builder(
            "redacted",
            format!("{}?token=secret-query-value", server.url()),
        )
        .header("Authorization", "Bearer secret-query-value")
        .build();
        assert!(!format!("{config:?}").contains("secret-query-value"));
        let report = discover_mcp_tools(&[config]).await.unwrap();
        assert_eq!(report.registrations().len(), 2);
        assert!(
            matches!(report.diagnostics()[0].issue(), McpServerIssue::Unavailable { failure, .. } if *failure == expected)
        );
        assert!(!format!("{:?}", report).contains("secret-query-value"));
        server.stop().await;
    }
}

#[tokio::test]
async fn redirects_are_not_followed_and_invalid_schemas_fail_only_their_server() {
    let target = Server::start(Mode::Online, vec![tool("read")]).await;
    let redirect = Server::start(Mode::Redirect(target.url()), vec![]).await;
    let report = discover_mcp_tools(&[redirect.config("redirect")])
        .await
        .unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(target.requests(), 0);

    let mut invalid = tool("invalid");
    invalid["inputSchema"] = json!({"type":"object","required":42});
    let invalid = Server::start(Mode::Online, vec![invalid]).await;
    let report = discover_mcp_tools(&[invalid.config("invalid"), target.config("target")])
        .await
        .unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].server_id().as_str(), "invalid");
    assert!(matches!(
        report.diagnostics()[0].issue(),
        McpServerIssue::Unavailable {
            failure: McpFailureKind::InvalidToolDefinition,
            ..
        }
    ));
    invalid.stop().await;
    redirect.stop().await;
    target.stop().await;
}

#[tokio::test]
async fn negotiates_counter_offered_strict_legacy_and_modern_protocol_versions() {
    for (name, mode) in [
        ("counter-offer", Mode::CounterOfferVersion("2025-06-18")),
        ("strict-legacy", Mode::StrictLegacyVersion("2025-03-26")),
        ("modern-2026", Mode::ModernDiscover),
    ] {
        let server = Server::start(mode, vec![tool("ping")]).await;
        let report = discover_mcp_tools(&[server.config(name)]).await.unwrap();
        assert!(
            report.diagnostics().is_empty(),
            "{name} should negotiate cleanly: {:?}",
            report.diagnostics()
        );
        let rt = runtime(name, report, &mcp_call_provider(name, "ping", 1));
        let call = next_call(&rt).await;
        let events = rt
            .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
            .await
            .unwrap();
        assert_eq!(resolved(&events).status(), ToolCallResultStatus::Succeeded);
        assert_eq!(server.calls(), 1);
        server.stop().await;
    }
}

#[tokio::test]
async fn sse_progress_notifications_and_multiline_tool_descriptions_are_handled_cleanly() {
    let mut multiline = tool("search");
    multiline["description"] = json!("First line\n\tSecond line\r\nThird line");
    let server = Server::start(Mode::SseWithProgress, vec![multiline]).await;

    let report = discover_mcp_tools(&[server.config("sse")]).await.unwrap();
    assert!(report.diagnostics().is_empty());

    let provider = sequence_provider(vec![
        mcp_search_call(json!({"server": "sse"})),
        (MCP_CALL_TOOL, mcp_call_args("sse", "search", json!({}))),
    ]);
    let rt = runtime("sse-progress", report, &provider);

    let search_call = next_call(&rt).await;
    let search_events = rt
        .execute_tool_call(
            &search_call,
            ToolExecutionContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    let search_json = read_json_output(&rt, resolved(&search_events)).await;
    assert_eq!(
        search_json["tools"][0]["description"],
        "First line Second line Third line"
    );

    let call = next_call(&rt).await;
    let events = rt
        .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
        .await
        .unwrap();
    let result = resolved(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        rt.read_tool_output(result.call_id())
            .await
            .expect("tool output read"),
        Some(ToolOutput::text("sse-done"))
    );
    server.stop().await;
}
