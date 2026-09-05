use super::support::*;
use crate::{McpError, McpFailureKind, McpServerConfig, McpServerIssue, discover_mcp_tools};
use std::time::Duration;

#[tokio::test]
async fn partial_and_total_outages_are_nonfatal_and_keep_healthy_tools() {
    let healthy = Server::start(Mode::Online, vec![tool("zeta"), tool("alpha")]).await;
    let offline = Server::start(Mode::Status(503), vec![]).await;
    let report = discover_mcp_tools(&[offline.config("offline"), healthy.config("healthy")])
        .await
        .unwrap();
    assert_eq!(
        report
            .registrations()
            .iter()
            .map(|registration| {
                registration
                    .tool
                    .external_binding()
                    .expect("MCP registrations have an external binding")
                    .operation()
                    .as_str()
            })
            .collect::<Vec<_>>(),
        ["alpha", "zeta"]
    );
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].server_id().as_str(), "offline");
    assert_eq!(report.diagnostics()[0].retained_tools(), 0);
    let report = discover_mcp_tools(&[offline.config("offline")])
        .await
        .unwrap();
    assert!(report.registrations().is_empty());
    assert!(matches!(
        report.diagnostics()[0].issue(),
        McpServerIssue::Unavailable {
            failure: McpFailureKind::Http(503),
            ..
        }
    ));
    healthy.stop().await;
    offline.stop().await;
}

#[tokio::test]
async fn tool_order_is_stable_even_when_pages_and_discovery_order_change() {
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
    assert_eq!(before.registrations().len(), 3);
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
    assert_eq!(healthy.requests(), 0);
    healthy.stop().await;
}

#[tokio::test]
async fn conflicting_server_tool_names_do_not_abort_other_servers() {
    let first = Server::start(Mode::Online, vec![tool("b_c")]).await;
    let second = Server::start(Mode::Online, vec![tool("c")]).await;
    let healthy = Server::start(Mode::Online, vec![tool("read")]).await;
    let report = discover_mcp_tools(&[
        first.config("a"),
        second.config("a_b"),
        healthy.config("healthy"),
    ])
    .await
    .unwrap();
    assert_eq!(report.registrations().len(), 1);
    assert_eq!(
        report.registrations()[0]
            .tool
            .external_binding()
            .expect("MCP registrations have an external binding")
            .source()
            .as_str(),
        "healthy"
    );
    assert_eq!(report.diagnostics().len(), 2);
    assert!(report.diagnostics().iter().all(|diagnostic| matches!(
        diagnostic.issue(),
        McpServerIssue::Unavailable {
            failure: McpFailureKind::InvalidToolDefinition,
            ..
        }
    )));
    first.stop().await;
    second.stop().await;
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
    assert_eq!(report.registrations().len(), 1);
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
    discovery.abort();
    assert!(discovery.await.unwrap_err().is_cancelled());
    assert_eq!(server.requests(), 1);
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
    assert!(report.registrations().is_empty());
    assert_eq!(report.diagnostics().len(), 12);
    assert_eq!(server.requests(), 4);
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
        assert!(report.registrations().is_empty());
        assert!(
            matches!(report.diagnostics()[0].issue(), McpServerIssue::Unavailable { failure, .. } if *failure == expected)
        );
        assert!(!format!("{:?}", report).contains("secret-query-value"));
        server.stop().await;
    }
}

#[tokio::test]
async fn redirects_are_not_followed_and_invalid_schemas_disable_only_their_server() {
    let target = Server::start(Mode::Online, vec![tool("read")]).await;
    let redirect = Server::start(Mode::Redirect(target.url()), vec![]).await;
    let report = discover_mcp_tools(&[redirect.config("redirect")])
        .await
        .unwrap();
    assert!(report.registrations().is_empty());
    assert_eq!(target.requests(), 0);
    let mut invalid = tool("invalid");
    invalid["inputSchema"] = serde_json::json!({"type":"object","required":42});
    let invalid = Server::start(Mode::Online, vec![invalid]).await;
    let report = discover_mcp_tools(&[invalid.config("invalid"), target.config("target")])
        .await
        .unwrap();
    assert_eq!(report.registrations().len(), 1);
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
