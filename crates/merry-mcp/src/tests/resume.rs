use super::support::*;
use crate::{McpServerConfig, McpServerIssue, discover_mcp_tools, restore_mcp_tools};
use merry_core::{
    ExternalToolBinding, SessionId, SessionToolCatalog, SessionToolCatalogEntry, ToolAdapterId,
    ToolBindingName, ToolCallResultStatus, ToolSourceFingerprint, ToolSourceId,
};
use merry_llm::ModelName;
use merry_runtime::{FileSessionStore, LoadedSession, Runtime, ToolExecutionContext};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn offline_resume_restores_saved_request_tools_and_stable_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(temp.path());
    let id = SessionId::new("offline-resume").unwrap();
    let server = Server::start(Mode::Online, vec![tool("zeta"), tool("alpha")]).await;
    let config = server.config("server");
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
    let loaded = LoadedSession::load(&store, &id).await.unwrap();
    let catalog = loaded.external_tool_catalog().clone();
    server.set_mode(Mode::Status(503));
    let discovery = restore_mcp_tools(&[config], &catalog).await.unwrap();
    assert_eq!(discovery.diagnostics()[0].retained_tools(), 2);
    let builder = Runtime::builder(id).fully_trusted_tools().model_provider(
        Arc::new(provider.clone()),
        ModelName::new("fixture").unwrap(),
    );
    let resumed = discovery
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
    assert_eq!(resumed.external_tool_catalog().await, catalog);
    server.stop().await;
}

#[tokio::test]
async fn initially_empty_catalog_does_not_gain_tools_after_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(temp.path());
    let server = Server::start(Mode::Status(503), vec![tool("late")]).await;
    let config = server.config("server");
    let original = runtime(
        "empty-recovery",
        discover_mcp_tools(std::slice::from_ref(&config))
            .await
            .unwrap(),
        &text_provider(),
    );
    original.save_session_to(store.clone()).await.unwrap();
    let loaded = LoadedSession::load(&store, original.session_id())
        .await
        .unwrap();
    let catalog = loaded.external_tool_catalog().clone();
    assert!(catalog.entries().is_empty());
    server.set_mode(Mode::Online);
    let requests = server.requests();
    let restored = restore_mcp_tools(&[config], &catalog).await.unwrap();
    assert!(restored.registrations().is_empty());
    assert!(matches!(
        restored.diagnostics()[0].issue(),
        McpServerIssue::NotInSessionCatalog
    ));
    assert_eq!(server.requests(), requests);
    server.stop().await;
}

#[tokio::test]
async fn restore_ignores_catalog_entries_owned_by_other_adapters() {
    let server = Server::start(Mode::Online, vec![tool("read")]).await;
    let config = server.config("server");
    let discovered = discover_mcp_tools(std::slice::from_ref(&config))
        .await
        .unwrap();
    let mcp_entry = discovered.registrations()[0]
        .tool
        .external_binding()
        .cloned()
        .map(|binding| {
            SessionToolCatalogEntry::new(discovered.registrations()[0].tool.spec().clone(), binding)
        })
        .unwrap();
    let other_entry = SessionToolCatalogEntry::new(
        merry_core::ToolSpec::new(
            merry_core::ToolName::new("other_tool").unwrap(),
            "Other adapter tool",
            discovered.registrations()[0]
                .tool
                .spec()
                .input_schema()
                .clone(),
        )
        .unwrap(),
        ExternalToolBinding::new(
            ToolAdapterId::new("other").unwrap(),
            ToolSourceId::new("other-source").unwrap(),
            ToolBindingName::new("other_operation").unwrap(),
            ToolSourceFingerprint::new("other-fingerprint").unwrap(),
        ),
    );
    let catalog = SessionToolCatalog::new(vec![mcp_entry, other_entry]).unwrap();
    server.set_mode(Mode::Status(503));
    let restored = restore_mcp_tools(&[config], &catalog).await.unwrap();
    assert_eq!(restored.registrations().len(), 1);
    assert_eq!(restored.diagnostics()[0].retained_tools(), 1);
    server.stop().await;
}

#[tokio::test]
async fn changed_definition_is_retained_but_never_executed_and_new_tools_are_omitted() {
    let server = Server::start(Mode::Online, vec![tool("read")]).await;
    let config = server.config("server");
    let original = runtime(
        "original",
        discover_mcp_tools(std::slice::from_ref(&config))
            .await
            .unwrap(),
        &text_provider(),
    );
    let catalog = original.external_tool_catalog().await;
    let mut changed = tool("read");
    changed["inputSchema"] = serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]});
    server.set_tools(vec![changed, tool("new")]);
    let report = restore_mcp_tools(&[config], &catalog).await.unwrap();
    assert_eq!(report.registrations().len(), 1);
    assert_eq!(
        report.registrations()[0].tool.spec(),
        catalog.entries()[0].spec()
    );
    assert!(matches!(
        report.diagnostics()[0].issue(),
        McpServerIssue::CatalogChanged
    ));
    let provider = call_provider(catalog.entries()[0].spec().name(), 1);
    let runtime = runtime("changed", report, &provider);
    let call = next_call(&runtime).await;
    let events = runtime
        .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(resolved(&events).status(), ToolCallResultStatus::Failed);
    assert_eq!(
        resolved(&events).diagnostic().unwrap().code(),
        "mcp_tool_definition_changed"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(server.calls(), 0);
    server.stop().await;
}

#[tokio::test]
async fn removed_servers_changed_endpoints_and_revoked_allowlists_never_execute() {
    let first = Server::start(Mode::Online, vec![tool("read")]).await;
    let other = Server::start(Mode::Online, vec![tool("read")]).await;
    let original = runtime(
        "original-grant",
        discover_mcp_tools(&[first.config("server")]).await.unwrap(),
        &text_provider(),
    );
    let catalog = original.external_tool_catalog().await;
    for (configs, expected) in [
        (vec![], McpServerIssue::NotConfigured),
        (
            vec![other.config("server")],
            McpServerIssue::EndpointChanged,
        ),
        (
            vec![
                McpServerConfig::builder("server", first.url())
                    .tools(Vec::<String>::new())
                    .build(),
            ],
            McpServerIssue::ToolsDisallowed,
        ),
    ] {
        let report = restore_mcp_tools(&configs, &catalog).await.unwrap();
        assert_eq!(report.registrations().len(), 1);
        assert_eq!(report.diagnostics()[0].issue(), &expected);
        let runtime = runtime(
            "revoked",
            report,
            &call_provider(catalog.entries()[0].spec().name(), 1),
        );
        let call = next_call(&runtime).await;
        let events = runtime
            .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
            .await
            .unwrap();
        assert_eq!(resolved(&events).status(), ToolCallResultStatus::Failed);
    }
    assert_eq!(first.calls(), 0);
    assert_eq!(other.requests(), 0);
    first.stop().await;
    other.stop().await;
}

#[tokio::test]
async fn a_new_name_collision_does_not_rename_the_saved_tool() {
    let server = Server::start(Mode::Online, vec![tool("read.file")]).await;
    let original = runtime(
        "name-origin",
        discover_mcp_tools(&[server.config("server")])
            .await
            .unwrap(),
        &text_provider(),
    );
    let catalog: SessionToolCatalog = original.external_tool_catalog().await;
    server.set_tools(vec![tool("read/file"), tool("read.file")]);
    let report = restore_mcp_tools(&[server.config("server")], &catalog)
        .await
        .unwrap();
    assert_eq!(report.registrations().len(), 1);
    assert_eq!(
        report.registrations()[0].tool.spec(),
        catalog.entries()[0].spec()
    );
    let runtime = runtime(
        "collision",
        report,
        &call_provider(catalog.entries()[0].spec().name(), 1),
    );
    let call = next_call(&runtime).await;
    let events = runtime
        .execute_tool_call(&call, ToolExecutionContext::new(CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(resolved(&events).status(), ToolCallResultStatus::Succeeded);
    assert_eq!(server.calls(), 1);
    server.stop().await;
}
